//! A browser adapter over the production replica, rather than a second CRDT.
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

use jade_collab_sync::{
    protocol::{Downstream, Resume, Welcome},
    Replica, ReplicaEvent,
};
use jade_docx_collab::{
    intent::{self, IntentOp},
    DocxModel, DocxOp,
};
use serde_json::{json, Value};

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct BrowserDocument {
    replica: Replica<DocxModel>,
}

fn utf16_byte(text: &str, offset: usize) -> Result<usize, String> {
    let mut units = 0;
    for (byte, ch) in text.char_indices() {
        if units == offset {
            return Ok(byte);
        }
        units += ch.len_utf16();
    }
    if units == offset {
        Ok(text.len())
    } else {
        Err("Selection is outside a Unicode boundary".into())
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl BrowserDocument {
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(constructor))]
    pub fn new(welcome: &str) -> Result<BrowserDocument, String> {
        let value: Value = serde_json::from_str(welcome).map_err(|e| e.to_string())?;
        if value.get("format").is_some_and(|f| f != "docx") {
            return Err("Only DOCX is supported by this demo".into());
        }
        let welcome: Welcome<DocxModel> =
            serde_json::from_value(value).map_err(|e| e.to_string())?;
        Ok(Self {
            replica: Replica::from_welcome(welcome),
        })
    }

    /// Resolve DOM UTF-16 carets in the local view, then publish the entire
    /// plan atomically. Invalid input never consumes IDs or submission tickets.
    pub fn replace(
        &mut self,
        paragraph: usize,
        start: usize,
        end: usize,
        text: &str,
    ) -> Result<String, String> {
        if start > end {
            return Err("Selection is reversed".into());
        }
        let paragraphs = self.replica.view().paragraphs();
        let p = paragraphs.get(paragraph).ok_or("Paragraph disappeared")?;
        let op = IntentOp::ReplaceRange {
            start_paragraph: paragraph,
            start_offset: utf16_byte(&p.text, start)?,
            end_paragraph: paragraph,
            end_offset: utf16_byte(&p.text, end)?,
            text: text.into(),
        };
        let mut staged = self.replica.clone();
        let view = staged.view().clone();
        let ops = intent::try_resolve(&view, &op, &mut |n| staged.try_reserve(n))
            .map_err(|e| e.to_string())?;
        let mut outgoing = Vec::new();
        for op in ops {
            let submission = staged.submit(op).map_err(|e| e.to_string())?;
            let mut wire = serde_json::to_value(submission).map_err(|e| e.to_string())?;
            wire["type"] = json!("submit");
            // Keep identifiers as raw JSON until WebSocket.send. A browser's
            // JSON.parse/stringify round trip would round u64s above 2^53.
            outgoing.push(wire.to_string());
        }
        let wire = serde_json::to_string(&outgoing).map_err(|e| e.to_string())?;
        self.replica = staged;
        Ok(wire)
    }

    /// Returns rejection reasons for the UI; the replica rolls back refusals.
    pub fn receive(&mut self, wire: &str) -> Result<String, String> {
        let value: Value = serde_json::from_str(wire).map_err(|e| e.to_string())?;
        let events = if value["type"] == "resumed" {
            let resume: Resume<DocxOp> =
                serde_json::from_value(value).map_err(|e| e.to_string())?;
            self.replica.resume(resume).map_err(|e| e.to_string())?
        } else {
            let message: Downstream<DocxOp> =
                serde_json::from_value(value).map_err(|e| e.to_string())?;
            vec![self.replica.receive(message).map_err(|e| e.to_string())?]
        };
        let reasons: Vec<_> = events
            .into_iter()
            .filter_map(|e| match e {
                ReplicaEvent::Rejected { reason, .. } => Some(reason.to_string()),
                _ => None,
            })
            .collect();
        serde_json::to_string(&reasons).map_err(|e| e.to_string())
    }

    pub fn resume_message(&self) -> String {
        json!({"type":"resume", "replica":self.replica.id(), "seq":self.replica.seq()}).to_string()
    }

    /// Compact raw-JSON anchors remain strings through JavaScript, preserving
    /// all u64 IDs. Presence is ephemeral and never consumes edit tickets.
    pub fn capture_selection(
        &self,
        ap: usize,
        au: usize,
        fp: usize,
        fu: usize,
    ) -> Result<String, String> {
        let view = self.replica.view();
        let paragraphs = view.paragraphs();
        let capture = |p: usize, units: usize| -> Result<String, String> {
            let text = &paragraphs.get(p).ok_or("Missing paragraph")?.text;
            let cursor = view
                .text_cursor(p, utf16_byte(text, units)?)
                .ok_or("Invalid cursor")?;
            serde_json::to_string(&cursor).map_err(|e| e.to_string())
        };
        Ok(json!({"anchor":capture(ap,au)?,"focus":capture(fp,fu)?}).to_string())
    }

    pub fn presence_id(&self, wire: &str) -> Result<String, String> {
        let value: Value = serde_json::from_str(wire).map_err(|e| e.to_string())?;
        value["replica"]
            .as_u64()
            .map(|id| id.to_string())
            .ok_or("Missing presence identity".into())
    }

    pub fn resolve_selection(&self, wire: &str) -> Result<String, String> {
        if wire.len() > 8192 {
            return Err("Presence selection is too large".into());
        }
        let selection: Value = serde_json::from_str(wire).map_err(|e| e.to_string())?;
        let view = self.replica.view();
        let paragraphs = view.paragraphs();
        let resolve = |key: &str| -> Result<Option<(usize, usize)>, String> {
            let cursor = serde_json::from_str(selection[key].as_str().ok_or("Missing cursor")?)
                .map_err(|e| e.to_string())?;
            Ok(view
                .resolve_text_cursor(&cursor)
                .map(|(p, byte)| (p, paragraphs[p].text[..byte].encode_utf16().count())))
        };
        Ok(match (resolve("anchor")?, resolve("focus")?) {
            (Some(a), Some(f)) => json!({"anchor":a,"focus":f}),
            _ => Value::Null,
        }
        .to_string())
    }

    pub fn pending(&self) -> Result<String, String> {
        let messages: Vec<_> = self
            .replica
            .pending_submissions()
            .into_iter()
            .map(|s| {
                let mut v = serde_json::to_value(s).expect("submission serializes");
                v["type"] = json!("submit");
                v.to_string()
            })
            .collect();
        serde_json::to_string(&messages).map_err(|e| e.to_string())
    }

    pub fn state(&self) -> Result<String, String> {
        let paragraphs: Vec<_> = self.replica.view().paragraphs().into_iter().enumerate().map(|(index,p)| {
            let chars: Vec<_> = p.chars.iter().zip(p.text.chars()).map(|(id,ch)| {
                json!({"id":format!("{}:{}", id.replica.0, id.lamport), "units":ch.len_utf16()})
            }).collect();
            json!({"index":index, "key":format!("{:?}/{:?}",p.story,p.mark), "text":p.text, "chars":chars, "story":format!("{:?}",p.story), "location":format!("{:?}",p.at)})
        }).collect();
        Ok(json!({"replica":self.replica.id().0.to_string(), "seq":self.replica.seq().to_string(), "pending":self.replica.pending_len(), "paragraphs":paragraphs}).to_string())
    }
}

#[cfg(test)]
mod tests;
