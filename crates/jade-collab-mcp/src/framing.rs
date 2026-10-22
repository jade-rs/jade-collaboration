//! Bounded, cancellation-safe newline framing. An overlong record is drained
//! through its delimiter before another record can be published.
use std::io;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Frame {
    Bytes(Vec<u8>),
    TooLarge,
}

#[derive(Default)]
struct Pending {
    bytes: Vec<u8>,
    dropping: bool,
}
impl Pending {
    fn push(&mut self, fragment: &[u8], limit: usize) {
        if self.dropping {
            return;
        }
        // One extra byte distinguishes an exact-limit CRLF record from an
        // overlong payload, even when CR and LF arrive in separate reads.
        if fragment.len() > limit.saturating_add(1).saturating_sub(self.bytes.len()) {
            self.bytes = Vec::new();
            self.dropping = true;
        } else {
            self.bytes.extend_from_slice(fragment);
        }
    }
    fn finish(&mut self, limit: usize, newline: bool) -> Frame {
        let mut bytes = std::mem::take(&mut self.bytes);
        let dropping = std::mem::replace(&mut self.dropping, false);
        if newline && bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
        if dropping || bytes.len() > limit {
            Frame::TooLarge
        } else {
            Frame::Bytes(bytes)
        }
    }
    fn has_input(&self) -> bool {
        self.dropping || !self.bytes.is_empty()
    }
}

pub(crate) struct Reader<R> {
    input: R,
    pending: Pending,
    limit: usize,
}
impl<R: AsyncBufRead + Unpin> Reader<R> {
    pub(crate) fn new(input: R, limit: usize) -> Self {
        Self {
            input,
            pending: Pending::default(),
            limit,
        }
    }
    pub(crate) async fn next(&mut self) -> io::Result<Option<Frame>> {
        loop {
            let input = self.input.fill_buf().await?;
            if input.is_empty() {
                return Ok(self
                    .pending
                    .has_input()
                    .then(|| self.pending.finish(self.limit, false)));
            }
            let end = input.iter().position(|b| *b == b'\n');
            let length = end.unwrap_or(input.len());
            self.pending.push(&input[..length], self.limit);
            self.input.consume(length + usize::from(end.is_some()));
            if end.is_some() {
                return Ok(Some(self.pending.finish(self.limit, true)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use tokio::io::{AsyncWriteExt, BufReader};

    fn model(bytes: &[u8], limit: usize) -> Vec<Frame> {
        let mut result = Vec::new();
        let mut current = Vec::new();
        for &byte in bytes {
            if byte == b'\n' {
                if current.last() == Some(&b'\r') {
                    current.pop();
                }
                result.push(if current.len() > limit {
                    Frame::TooLarge
                } else {
                    Frame::Bytes(current.clone())
                });
                current.clear();
            } else {
                current.push(byte);
            }
        }
        if !current.is_empty() {
            result.push(if current.len() > limit {
                Frame::TooLarge
            } else {
                Frame::Bytes(current)
            });
        }
        result
    }

    #[tokio::test]
    async fn embedded_message_handler_applies_the_same_bound_before_trimming_or_dispatch() {
        let mut server = crate::McpServer::new();
        let oversized = " ".repeat(crate::MAX_REQUEST_BYTES + 1);
        let response = server
            .handle_message(&oversized)
            .await
            .expect("oversized input is refused");
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(response["id"].is_null());
        assert_eq!(response["error"]["code"], -32600);
        let next = server
            .handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&next).unwrap()["id"],
            1
        );
    }

    #[tokio::test]
    async fn exact_bound_crlf_unicode_and_unterminated_eof_keep_their_record_boundaries() {
        for input in [
            b"1234\r\nnext\n".as_slice(),
            "🙂\r\nlast".as_bytes(),
            b"12345\nnext\n",
            b"1234\r",
            b"\xff\nokay\n",
            b"\r\n\n",
        ] {
            let mut reader = Reader::new(BufReader::with_capacity(1, input), 4);
            let mut actual = Vec::new();
            while let Some(frame) = reader.next().await.unwrap() {
                actual.push(frame);
            }
            assert_eq!(actual, model(input, 4));
        }
    }

    #[tokio::test]
    async fn canceling_a_partial_read_retains_its_prefix_and_overflow_state() {
        for prefix in [b"ab".as_slice(), b"too-long-for-the-record"] {
            let (mut sender, receiver) = tokio::io::duplex(128);
            let mut reader = Reader::new(BufReader::new(receiver), 4);
            sender.write_all(prefix).await.unwrap();
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(10), reader.next())
                    .await
                    .is_err()
            );
            assert!(
                reader.pending.has_input(),
                "the canceled call consumed the prefix"
            );
            assert_eq!(reader.pending.dropping, prefix != b"ab");
            sender.write_all(b"cd\nnext\n").await.unwrap();
            drop(sender);
            assert_eq!(
                reader.next().await.unwrap(),
                Some(if prefix == b"ab" {
                    Frame::Bytes(b"abcd".to_vec())
                } else {
                    Frame::TooLarge
                })
            );
            assert_eq!(
                reader.next().await.unwrap(),
                Some(Frame::Bytes(b"next".to_vec()))
            );
            assert_eq!(reader.next().await.unwrap(), None);
        }
    }

    proptest! {
        #[test]
        fn fragmented_record_histories_match_the_unbounded_reference_without_unbounded_retention(
            limit in 0usize..128,
            records in prop::collection::vec((prop::collection::vec(any::<u8>().prop_filter("not LF",|b|*b!=b'\n'),0..200),any::<bool>()),0..24),
            capacity in 1usize..47,final_newline in any::<bool>(),
        ) {
            let mut input=Vec::new();
            let count=records.len();
            for (i,(record,crlf)) in records.into_iter().enumerate() {
                input.extend(record);
                if i+1<count || final_newline {if crlf {input.push(b'\r');}input.push(b'\n');}
            }
            let expected=model(&input,limit);
            tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
                let mut reader=Reader::new(BufReader::with_capacity(capacity,input.as_slice()),limit);let mut actual=Vec::new();
                while let Some(frame)=reader.next().await.unwrap() {actual.push(frame);assert!(reader.pending.bytes.len()<=limit+1);}
                assert_eq!(actual,expected);
            });
            // Exercise push/finish directly so the retention invariant is
            // checked while an incomplete record is being drained too.
            let mut pending=Pending::default();
            for byte in input {
                if byte==b'\n' {pending.finish(limit,true);} else {pending.push(&[byte],limit);}
                prop_assert!(pending.bytes.len()<=limit+1);
                if pending.dropping {prop_assert!(pending.bytes.is_empty());}
            }
        }
    }
}
