use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Counter(u64);
impl Document for Counter {
    type Op = bool;
    type Effect = ();
    fn lane(_: &bool) -> jade_collab_sync::Lane {
        jade_collab_sync::Lane::Convergent
    }
    fn apply(&mut self, op: &bool, _: &jade_collab_sync::ApplyContext) -> Result<(), Rejection> {
        if !op {
            return Err(Rejection::new("invalid"));
        }
        self.0 += 1;
        Ok(())
    }
    fn max_lamport(&self) -> u64 {
        0
    }
}

struct TestOutbound<D: Document> {
    shared: Arc<Shared<D>>,
    cursor: pump::Cursor,
}
impl<D> TestOutbound<D>
where
    D: Document + DeserializeOwned + Send + 'static,
    D::Op: Serialize + DeserializeOwned + Send + 'static,
{
    fn try_recv(&mut self) -> Result<String, ()> {
        match self.cursor.message(&self.shared).unwrap() {
            Some(Message::Text(message)) => Ok(message.to_string()),
            _ => Err(()),
        }
    }
    async fn recv(&mut self) -> Option<String> {
        loop {
            let shared = self.shared.clone();
            let notified = shared.outgoing.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Ok(message) = self.try_recv() {
                return Some(message);
            }
            if shared.error.lock().unwrap().is_some() {
                return None;
            }
            notified.await;
        }
    }
}
fn client() -> (Client<Counter>, TestOutbound<Counter>) {
    client_with(Counter(0))
}

fn client_with<D: Document>(document: D) -> (Client<D>, TestOutbound<D>) {
    let shared = Arc::new(Shared {
        identity: Arc::new(()),
        replica: Mutex::new(Replica::new(ReplicaId(1), 0, document)),
        events: Mutex::new(Vec::new()),
        presence: Mutex::new(BTreeMap::new()),
        presence_rejections: Mutex::new(Vec::new()),
        error: Mutex::new(None),
        inspections: Mutex::new(InspectionRequests::default()),
        changed: Notify::new(),
        outgoing: Notify::new(),
        latest_presence: Mutex::new(None),
    });
    (
        Client {
            shared: shared.clone(),
            tasks: ConnectionTasks(Vec::new()),
        },
        TestOutbound {
            shared,
            cursor: pump::Cursor::new(1),
        },
    )
}

#[test]
fn unsent_presence_updates_keep_only_the_latest_value() {
    let (client, mut outbound) = client();
    for cursor in 0..1000 {
        client.set_presence(serde_json::json!({"cursor":cursor}));
    }
    let message: ClientMessage = serde_json::from_str(&outbound.try_recv().unwrap()).unwrap();
    let ClientMessage::Presence { state } = message else {
        panic!("presence")
    };
    assert_eq!(state, serde_json::json!({"cursor":999}));
    assert!(outbound.try_recv().is_err());
}

#[derive(Clone, Debug)]
struct ObservedOp(Arc<std::sync::atomic::AtomicUsize>);
impl PartialEq for ObservedOp {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Serialize for ObservedOp {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        serializer.serialize_bool(true)
    }
}
impl<'de> Deserialize<'de> for ObservedOp {
    fn deserialize<T: serde::Deserializer<'de>>(d: T) -> Result<Self, T::Error> {
        bool::deserialize(d)?;
        Ok(Self(Arc::new(std::sync::atomic::AtomicUsize::new(0))))
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Observed(u64);
impl Document for Observed {
    type Op = ObservedOp;
    type Effect = ();
    fn lane(_: &ObservedOp) -> jade_collab_sync::Lane {
        jade_collab_sync::Lane::Convergent
    }
    fn apply(
        &mut self,
        _: &ObservedOp,
        _: &jade_collab_sync::ApplyContext,
    ) -> Result<(), Rejection> {
        self.0 += 1;
        Ok(())
    }
    fn max_lamport(&self) -> u64 {
        0
    }
}

#[test]
fn queued_edits_and_aborted_plans_do_not_allocate_serialized_wire_copies() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let encoded = Arc::new(AtomicUsize::new(0));
    let (client, mut outbound) = client_with(Observed(0));
    for _ in 0..1000 {
        client.submit(ObservedOp(encoded.clone())).unwrap();
    }
    assert_eq!(
        encoded.load(Ordering::SeqCst),
        0,
        "the pending operations are the send source"
    );
    assert_eq!(client.pending_len(), 1000);
    outbound.try_recv().unwrap();
    assert_eq!(encoded.load(Ordering::SeqCst), 1);
    let before = client.snapshot();
    let result = client.try_edit(None, |context| {
        context.try_reserve(10).unwrap();
        context.submit(ObservedOp(encoded.clone())).unwrap();
        Err::<(), _>("abort")
    });
    assert!(result.is_err());
    assert_eq!(client.snapshot().pending, before.pending);
    assert_eq!(
        encoded.load(Ordering::SeqCst),
        1,
        "refused plans never enter the writer"
    );
}

#[tokio::test]
async fn a_blocked_writer_holds_one_encoded_edit_and_inspects_before_later_edits() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let encoded = Arc::new(AtomicUsize::new(0));
    let (client, outbound) = client_with(Observed(0));
    drop(outbound);
    for _ in 0..1000 {
        client.submit(ObservedOp(encoded.clone())).unwrap();
    }
    let inspection = client.inspect();
    tokio::pin!(inspection);
    assert!(futures_util::poll!(&mut inspection).is_pending());
    client.submit(ObservedOp(encoded.clone())).unwrap();
    let permits = Arc::new(tokio::sync::Semaphore::new(0));
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
    let sink = Box::pin(futures_util::sink::unfold(
        (permits.clone(), sent),
        |(permits, sent), message| async move {
            permits.acquire().await.unwrap().forget();
            sent.send(message).unwrap();
            Ok::<_, std::io::Error>((permits, sent))
        },
    ));
    let writer = tokio::spawn(pump::write(
        client.shared.clone(),
        pump::Cursor::new(1),
        sink,
    ));
    tokio::time::timeout(Duration::from_secs(2), async {
        while encoded.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    for cursor in 0..1000 {
        client.set_presence(serde_json::json!(cursor));
    }
    tokio::task::yield_now().await;
    assert_eq!(
        encoded.load(Ordering::SeqCst),
        1,
        "only the blocked edit is encoded"
    );
    assert_eq!(client.pending_len(), 1001);
    permits.add_permits(1003); // edits + inspection + coalesced presence
    let mut tickets = Vec::new();
    let mut presences = Vec::new();
    let mut inspected_at = None;
    for _ in 0..1003 {
        let Message::Text(message) = tokio::time::timeout(Duration::from_secs(2), received.recv())
            .await
            .unwrap()
            .unwrap()
        else {
            panic!("text")
        };
        match serde_json::from_str::<ClientMessage>(&message).unwrap() {
            ClientMessage::Submit(submission) => tickets.push(submission.client_seq),
            ClientMessage::Inspect { .. } => inspected_at = Some(tickets.len()),
            ClientMessage::Presence { state } => presences.push(state),
            _ => panic!("unexpected message"),
        }
    }
    assert_eq!(tickets, (1..=1001).collect::<Vec<_>>());
    assert_eq!(inspected_at, Some(1000));
    assert_eq!(presences, vec![serde_json::json!(999)]);
    assert_eq!(encoded.load(Ordering::SeqCst), 1001);
    writer.abort();
    writer.await.unwrap_err();
}

#[tokio::test]
async fn send_failure_wakes_settlement_and_inspections_and_keeps_the_retry_source() {
    let (client, outbound) = client();
    drop(outbound);
    client.submit(true).unwrap();
    let inspection = client.inspect();
    tokio::pin!(inspection);
    assert!(futures_util::poll!(&mut inspection).is_pending());
    let sink = Box::pin(futures_util::sink::unfold((), |(), _: Message| async {
        Err::<(), _>(std::io::Error::other("injected write failure"))
    }));
    // There is deliberately no reader delivering a close/error. Write failure
    // alone must terminate every waiter.
    pump::write(client.shared.clone(), pump::Cursor::new(1), sink).await;
    assert!(
        matches!(tokio::time::timeout(Duration::from_secs(1), &mut inspection).await.unwrap(), Err(ClientError::Protocol(message)) if message.contains("injected write failure"))
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(1), client.settle())
            .await
            .unwrap()
            .is_err()
    );
    assert!(client.shared.inspections.lock().unwrap().waiting.is_empty());
    client.shared.fail(ClientError::Closed);
    assert!(matches!(
        client.connection_error(),
        Some(ClientError::Protocol(_))
    ));
    client.submit(true).unwrap(); // offline optimistic editing retains retryable ops
    assert_eq!(
        client
            .shared
            .replica
            .lock()
            .unwrap()
            .pending_submissions()
            .iter()
            .map(|s| s.client_seq)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let mut cursor = pump::Cursor::new(1);
    assert!(cursor.message(&client.shared).unwrap().is_none());
}

#[tokio::test]
async fn a_reader_failure_cancels_a_writer_blocked_on_send() {
    let (client, outbound) = client();
    drop(outbound);
    client.submit(true).unwrap();
    let (entered, ready) = oneshot::channel();
    let sink = Box::pin(futures_util::sink::unfold(
        Some(entered),
        |mut entered, _: Message| async move {
            entered.take().unwrap().send(()).unwrap();
            std::future::pending::<()>().await;
            Ok::<_, std::io::Error>(entered)
        },
    ));
    let writer = tokio::spawn(pump::write(
        client.shared.clone(),
        pump::Cursor::new(1),
        sink,
    ));
    ready.await.unwrap();
    client
        .shared
        .fail(ClientError::Revoked("access removed".into()));
    tokio::time::timeout(Duration::from_secs(1), writer)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(client.pending_len(), 1);
    assert!(matches!(
        client.connection_error(),
        Some(ClientError::Revoked(_))
    ));
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct Unserializable;
impl Serialize for Unserializable {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("injected encoding failure"))
    }
}
impl Document for Unserializable {
    type Op = Self;
    type Effect = ();
    fn lane(_: &Self) -> jade_collab_sync::Lane {
        jade_collab_sync::Lane::Convergent
    }
    fn apply(&mut self, _: &Self, _: &jade_collab_sync::ApplyContext) -> Result<(), Rejection> {
        Ok(())
    }
    fn max_lamport(&self) -> u64 {
        0
    }
}

#[tokio::test]
async fn encoding_failure_is_terminal_without_poisoning_or_dropping_pending_edits() {
    let (client, outbound) = client_with(Unserializable);
    drop(outbound);
    client.submit(Unserializable).unwrap();
    let sink = futures_util::sink::drain::<Message>();
    pump::write(client.shared.clone(), pump::Cursor::new(1), sink).await;
    assert!(
        matches!(client.connection_error(), Some(ClientError::Protocol(message)) if message.contains("injected encoding failure"))
    );
    assert!(client.settle().await.is_err());
    assert_eq!(client.pending_len(), 1);
    assert!(
        client.try_reserve(1).is_ok(),
        "encoding did not poison the replica lock"
    );
    let source = client.disconnect().await;
    assert_eq!(source.pending_len(), 1);
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(128))]
    #[test]
    fn outbound_pump_matches_fifo_barriers_coalescing_and_failure_histories(
        steps in proptest::collection::vec((0u8..10, proptest::num::u16::ANY), 1..160),
    ) {
        use proptest::prelude::*;
        use std::collections::VecDeque;
        #[derive(Clone, Debug, PartialEq)]
        enum Expected { Edit(u64), Inspect(u64) }
        let (client, mut outbound) = client();
        let mut queue = VecDeque::new();
        let mut replies = VecDeque::new();
        let mut inspections = BTreeMap::new();
        let (mut next, mut request, mut failed, mut last_presence) = (1, 0, false, false);
        let mut presence = None;
        for (kind, choice) in steps {
            match kind {
                0 | 1 => {
                    prop_assert_eq!(client.submit(true).unwrap(), next);
                    queue.push_back(Expected::Edit(next)); next += 1;
                }
                2 => {
                    let before = client.snapshot();
                    let refused = client.try_edit(None, |edit| { edit.submit(true)?; Err::<(),_>(Rejection::new("abort")) });
                    prop_assert!(refused.is_err()); prop_assert_eq!(client.snapshot(), before);
                }
                3 => { presence = Some(choice); client.set_presence(serde_json::json!(choice)); }
                4 if !failed => {
                    request += 1;
                    let (answer, receive) = oneshot::channel();
                    client.shared.inspections.lock().unwrap().waiting.insert(request, PendingInspection { next_client_seq: next, sent: false, answer });
                    inspections.insert(request, receive); queue.push_back(Expected::Inspect(request));
                }
                5 if !inspections.is_empty() => {
                    let request = *inspections.keys().nth(usize::from(choice) % inspections.len()).unwrap();
                    inspections.remove(&request);
                    client.shared.inspections.lock().unwrap().waiting.remove(&request);
                    queue.retain(|entry| *entry != Expected::Inspect(request));
                }
                6 | 7 => {
                    let expected_presence = !failed && presence.is_some() && (!last_presence || queue.is_empty());
                    let message = outbound.try_recv().ok().map(|wire| serde_json::from_str::<ClientMessage>(&wire).unwrap());
                    if failed { prop_assert!(message.is_none()); }
                    else if expected_presence {
                        let Some(ClientMessage::Presence { state }) = message else { prop_assert!(false, "expected presence"); unreachable!() };
                        prop_assert_eq!(state, serde_json::json!(presence.take().unwrap())); last_presence = true;
                    } else if let Some(expected) = queue.pop_front() {
                        last_presence = false;
                        match (expected, message) {
                            (Expected::Edit(ticket), Some(ClientMessage::Submit(submission))) => {
                                prop_assert_eq!(submission.client_seq, ticket); replies.push_back(ticket);
                            }
                            (Expected::Inspect(expected), Some(ClientMessage::Inspect { request })) => { prop_assert_eq!(request, expected); }
                            (expected, got) => { prop_assert!(false, "expected {:?}, got {:?}", expected, got); }
                        }
                    } else { prop_assert!(message.is_none()); last_presence = false; }
                }
                8 if !failed => if let Some(ticket) = replies.pop_front() {
                    Client::handle(&client.shared, ServerMessage::Rejected(jade_collab_sync::protocol::Rejected { client_seq: ticket, reason: Rejection::new("refused") })).unwrap();
                },
                9 if !failed && choice % 7 == 0 => { failed = true; client.shared.fail(ClientError::Closed); inspections.clear(); }
                _ => {}
            }
            prop_assert_eq!(client.shared.replica.lock().unwrap().next_client_seq(), next);
            prop_assert_eq!(client.pending_len(), queue.iter().filter(|entry| matches!(entry, Expected::Edit(_))).count() + replies.len());
            prop_assert_eq!(client.view().0 as usize, client.pending_len());
        }
    }
}

#[tokio::test]
async fn an_unresponsive_upgrade_or_welcome_has_a_bounded_lifetime() {
    use std::time::Duration;
    use tokio::io::AsyncReadExt;
    for upgraded in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "ws://{}/documents/counter.docx/ws",
            listener.local_addr().unwrap()
        );
        let peer = tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            if upgraded {
                let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
                assert!(matches!(socket.next().await, Some(Ok(Message::Text(_)))));
                while let Some(message) = socket.next().await {
                    if message.is_err() {
                        break;
                    }
                }
            } else {
                let mut request = Vec::new();
                tcp.read_to_end(&mut request).await.unwrap();
                assert!(request.starts_with(b"GET "));
            }
        });
        let answer = tokio::time::timeout(
            Duration::from_secs(3),
            Client::<Counter>::connect_with_timeout(&url, None, Duration::from_millis(100)),
        )
        .await
        .expect("opening has a deadline");
        assert!(matches!(answer, Err(ClientError::Timeout(_))));
        tokio::time::timeout(Duration::from_secs(3), peer)
            .await
            .expect("timeout dropped the socket")
            .unwrap();
    }
}

#[test]
fn a_rejected_optimistic_edit_cannot_make_an_old_snapshot_current_again() {
    let (client, _outbound) = client();
    let before = client.snapshot();
    client.submit(true).unwrap();
    client
        .shared
        .replica
        .lock()
        .unwrap()
        .receive(Downstream::Rejected(jade_collab_sync::protocol::Rejected {
            client_seq: 1,
            reason: Rejection::new("server refused"),
        }))
        .unwrap();
    let after = client.snapshot();
    assert_eq!(before.view.0, after.view.0);
    assert_eq!((before.seq, before.pending), (after.seq, after.pending));
    assert!(client
        .edit(Some(&before), |_| panic!(
            "the old version must not be reused"
        ))
        .is_err());
    assert_eq!(
        client
            .edit(Some(&after), |context| context.submit(true))
            .unwrap()
            .unwrap(),
        2
    );
}

#[test]
fn concurrent_local_submissions_are_queued_in_client_sequence_order() {
    let (client, mut outbound) = client();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let client = &client;
            scope.spawn(move || {
                for step in 0..100 {
                    if step % 2 == 0 {
                        client
                            .try_edit(None, |context| {
                                let first = context.submit(true)?;
                                let second = context.submit(true)?;
                                assert_eq!(second, first + 1);
                                Ok::<_, Rejection>(())
                            })
                            .unwrap();
                    } else {
                        client.submit(true).unwrap();
                    }
                }
            });
        }
    });
    for expected in 1..=1200 {
        let message: ClientMessage = serde_json::from_str(&outbound.try_recv().unwrap()).unwrap();
        let ClientMessage::Submit(submission) = message else {
            panic!("expected submission")
        };
        assert_eq!(submission.client_seq, expected);
    }
    assert!(outbound.try_recv().is_err());
}

#[test]
fn failed_edit_plans_preserve_the_replica_clock_tickets_and_outgoing_queue() {
    let (client, mut outbound) = client();
    client.submit(true).unwrap();
    outbound.try_recv().unwrap();
    let before = client.snapshot();
    let expected_id = client
        .shared
        .replica
        .lock()
        .unwrap()
        .clone()
        .try_reserve(1)
        .unwrap();
    let result = client.try_edit(Some(&before), |context| {
        context.try_reserve(4).unwrap();
        context.submit(true)?;
        context.submit(false)?;
        Ok::<_, Rejection>(())
    });
    assert!(matches!(result, Err(EditError::Refused(_))));
    assert_eq!(client.snapshot(), before);
    assert!(outbound.try_recv().is_err());
    assert_eq!(client.try_reserve(1).unwrap(), expected_id);
    assert_eq!(
        client
            .try_edit(Some(&before), |context| context.submit(true))
            .unwrap(),
        2
    );
    let message: ClientMessage = serde_json::from_str(&outbound.try_recv().unwrap()).unwrap();
    let ClientMessage::Submit(submission) = message else {
        panic!("submission")
    };
    assert_eq!(submission.client_seq, 2);
    assert_eq!(client.view().0, 2);
    assert_eq!(client.pending_len(), 2);
}

#[test]
fn stale_edit_plans_never_invoke_the_planner() {
    let (client, _) = client();
    let old = client.snapshot();
    client.submit(true).unwrap();
    let result = client.try_edit(Some(&old), |_| -> Result<(), ()> {
        panic!("stale planner ran")
    });
    assert!(matches!(result, Err(EditError::Stale(_))));
}

#[tokio::test]
async fn authoritative_inspection_is_versioned_at_request_time_and_preserves_future_edits() {
    let (client, mut outbound) = client();
    let client = Arc::new(client);
    client.submit(true).unwrap();
    outbound.recv().await.unwrap();
    let inspecting = client.clone();
    let request = tokio::spawn(async move { inspecting.inspect().await });
    let message: ClientMessage = serde_json::from_str(&outbound.recv().await.unwrap()).unwrap();
    let ClientMessage::Inspect { request: id } = message else {
        panic!("inspection request")
    };
    // This edit is after the request and must not enter its server snapshot.
    client.submit(true).unwrap();
    outbound.recv().await.unwrap();
    Client::handle(
        &client.shared,
        ServerMessage::Committed(jade_collab_sync::protocol::Committed {
            seq: 1,
            origin: ReplicaId(1),
            client_seq: 1,
            base_seq: 0,
            op: serde_json::json!(true),
        }),
    )
    .unwrap();
    Client::handle(
        &client.shared,
        ServerMessage::Inspected {
            request: id,
            seq: 1,
            snapshot: serde_json::json!(1),
            formatting: serde_json::json!({"runs":[]}),
        },
    )
    .unwrap();
    let inspected = request.await.unwrap().unwrap();
    assert_eq!(
        (
            inspected.snapshot.view.0,
            inspected.snapshot.seq,
            inspected.snapshot.pending
        ),
        (1, 1, 0)
    );
    assert_eq!(client.view().0, 2);
    assert!(client
        .edit(Some(&inspected.snapshot), |_| panic!(
            "future edit invalidates guard"
        ))
        .is_err());
    assert_eq!(client.pending_len(), 1);
    Client::handle(
        &client.shared,
        ServerMessage::Rejected(jade_collab_sync::protocol::Rejected {
            client_seq: 2,
            reason: Rejection::new("later edit refused"),
        }),
    )
    .unwrap();
    assert_eq!(
        (client.view().0, client.seq(), client.pending_len()),
        (1, 1, 0)
    );
    assert!(client
        .edit(Some(&inspected.snapshot), |_| panic!(
            "the request ticket must survive a later rejection"
        ))
        .is_err());
}

#[tokio::test]
async fn cancelling_inspection_removes_the_request_and_late_answers_cannot_complete_another() {
    let (client, mut outbound) = client();
    let client = Arc::new(client);
    let worker = client.clone();
    let first = tokio::spawn(async move { worker.inspect().await });
    let message: ClientMessage = serde_json::from_str(&outbound.recv().await.unwrap()).unwrap();
    let ClientMessage::Inspect { request: old } = message else {
        panic!("request")
    };
    assert_eq!(client.shared.inspections.lock().unwrap().waiting.len(), 1);
    first.abort();
    let _ = first.await;
    assert!(client.shared.inspections.lock().unwrap().waiting.is_empty());
    let worker = client.clone();
    let second = tokio::spawn(async move { worker.inspect().await });
    let message: ClientMessage = serde_json::from_str(&outbound.recv().await.unwrap()).unwrap();
    let ClientMessage::Inspect { request: current } = message else {
        panic!("request")
    };
    assert!(current > old);
    Client::handle(
        &client.shared,
        ServerMessage::Inspected {
            request: old,
            seq: 999,
            snapshot: serde_json::json!(999),
            formatting: serde_json::json!({}),
        },
    )
    .unwrap();
    assert!(!second.is_finished());
    Client::handle(
        &client.shared,
        ServerMessage::Inspected {
            request: current,
            seq: 0,
            snapshot: serde_json::json!(0),
            formatting: serde_json::json!({}),
        },
    )
    .unwrap();
    let read = second.await.unwrap().unwrap();
    assert_eq!(read.snapshot.seq, 0);
    assert_eq!(read.snapshot.view.0, 0);
    assert!(client.shared.inspections.lock().unwrap().waiting.is_empty());
    assert_eq!(client.shared.replica.lock().unwrap().next_client_seq(), 1);
    client.shared.inspections.lock().unwrap().next = u64::MAX;
    assert!(matches!(
        client.inspect().await,
        Err(ClientError::Inspection(_))
    ));
    assert!(outbound.try_recv().is_err());
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(48))]
    #[test]
    fn inspection_requests_remain_correlated_and_bounded_through_cancellation_and_late_answers(
        steps in proptest::collection::vec((0u8..6, proptest::num::usize::ANY), 1..40),
    ) {
        use proptest::prelude::*;
        static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
        RUNTIME.get_or_init(|| tokio::runtime::Runtime::new().unwrap()).block_on(async {
            let (client, mut outbound) = client();
            let client = Arc::new(client);
            let mut requests = BTreeMap::new();
            let (mut last_request, mut seq) = (0, 0);
            for (kind, choice) in steps {
                match kind {
                    0 => {
                        let worker = client.clone();
                        let job = tokio::spawn(async move { worker.inspect().await });
                        let message: ClientMessage = serde_json::from_str(&outbound.recv().await.unwrap()).unwrap();
                        let ClientMessage::Inspect { request } = message else { panic!("inspection") };
                        prop_assert_eq!(request, last_request + 1); last_request = request;
                        requests.insert(request, (seq, job));
                    }
                    1..=3 if !requests.is_empty() => {
                        let request = *requests.keys().nth(choice % requests.len()).unwrap();
                        let (captured_seq, job) = requests.remove(&request).unwrap();
                        match kind {
                            1 => {
                                Client::handle(&client.shared, ServerMessage::Inspected { request, seq: captured_seq,
                                    snapshot: serde_json::json!(captured_seq), formatting: serde_json::json!(request) }).unwrap();
                                let got = job.await.unwrap().unwrap();
                                prop_assert_eq!(got.snapshot.seq, captured_seq);
                                prop_assert_eq!(got.snapshot.view.0, captured_seq);
                                prop_assert_eq!(got.formatting, serde_json::json!(request));
                                prop_assert_eq!(client.edit(Some(&got.snapshot), |_| ()).is_ok(), seq == captured_seq);
                            }
                            2 => {
                                job.abort(); let _ = job.await;
                                Client::handle(&client.shared, ServerMessage::Inspected { request, seq: 999,
                                    snapshot: serde_json::json!("late malformed payload"), formatting: serde_json::json!({}) }).unwrap();
                            }
                            _ => {
                                Client::handle(&client.shared, ServerMessage::InspectionFailed { request, message: "refused".into() }).unwrap();
                                prop_assert!(matches!(job.await.unwrap(), Err(ClientError::Inspection(_))));
                            }
                        }
                    }
                    4 => {
                        let ticket = client.submit(true).unwrap();
                        outbound.recv().await.unwrap();
                        let base_seq = seq; seq += 1;
                        Client::handle(&client.shared, ServerMessage::Committed(jade_collab_sync::protocol::Committed {
                            seq, origin: ReplicaId(1), client_seq: ticket, base_seq, op: serde_json::json!(true),
                        })).unwrap();
                    }
                    _ => {
                        Client::handle(&client.shared, ServerMessage::Inspected { request: u64::MAX, seq: 999,
                            snapshot: serde_json::json!("unknown"), formatting: serde_json::json!({}) }).unwrap();
                    }
                }
                prop_assert_eq!(client.shared.inspections.lock().unwrap().waiting.len(), requests.len());
                prop_assert_eq!(client.seq(), seq);
                prop_assert_eq!(client.view().0, seq);
                prop_assert_eq!(client.pending_len(), 0);
                prop_assert!(outbound.try_recv().is_err());
            }
            for (_, (_, job)) in requests { job.abort(); let _ = job.await; }
            prop_assert!(client.shared.inspections.lock().unwrap().waiting.is_empty());
            Ok(())
        })?;
    }
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(96))]
    #[test]
    fn fallible_edit_plans_match_a_state_machine_through_answers_and_retries(
        history in proptest::collection::vec((0u8..7, proptest::collection::vec((0u8..3, 0u64..5), 0..7), proptest::bool::ANY), 1..70),
    ) {
        use proptest::prelude::*;
        use std::collections::VecDeque;
        let (client, mut outbound) = client();
        let mut captured = client.snapshot();
        let (mut captured_version, mut seq, mut ticket, mut clock, mut confirmed) = ((0, 0, 1), 0, 1, 0, 0);
        let mut pending: VecDeque<(u64, u64)> = VecDeque::new();
        for (kind, actions, abort) in history {
            let mut published = Vec::new();
            match kind {
                0 => {
                    let result = client.submit(!abort);
                    prop_assert_eq!(result.is_ok(), !abort);
                    if !abort { published.push((ticket, seq)); pending.push_back((ticket, seq)); ticket += 1; }
                }
                1 | 2 => {
                    let current = client.snapshot();
                    let expected = if kind == 1 { &current } else { &captured };
                    let matches = kind == 1 || captured_version == (seq, pending.len(), ticket);
                    let valid = !abort && actions.iter().all(|(kind, n)| *kind != 2 && (*kind != 0 || *n != 0));
                    let mut plan_clock = clock;
                    let mut plan_ticket = ticket;
                    let mut ids = Vec::new();
                    let mut submissions = Vec::new();
                    if matches && valid {
                        for (kind, n) in &actions {
                            if *kind == 0 { ids.push(Id::new(plan_clock + 1, ReplicaId(1))); plan_clock += n; }
                            else { submissions.push(plan_ticket); plan_ticket += 1; }
                        }
                    }
                    let result = client.try_edit(Some(expected), |context| {
                        let mut ids = Vec::new();
                        let mut submitted = Vec::new();
                        for (kind, n) in &actions {
                            match kind {
                                0 => ids.push(context.try_reserve(*n).map_err(|e| e.to_string())?),
                                1 => submitted.push(context.submit(true).map_err(|e| e.to_string())?),
                                _ => { context.submit(false).map_err(|e| e.to_string())?; }
                            }
                        }
                        if abort { return Err("cancelled".to_owned()); }
                        Ok((ids, submitted))
                    });
                    if !matches { prop_assert!(matches!(result, Err(EditError::Stale(_)))); }
                    else if !valid { prop_assert!(matches!(result, Err(EditError::Refused(_)))); }
                    else {
                        prop_assert_eq!(result.unwrap(), (ids, submissions.clone()));
                        clock = plan_clock; ticket = plan_ticket;
                        for ticket in submissions { published.push((ticket, seq)); pending.push_back((ticket, seq)); }
                    }
                }
                3 => { captured = client.snapshot(); captured_version = (seq, pending.len(), ticket); }
                4 | 5 => if let Some((client_seq, base_seq)) = pending.pop_front() {
                    let message = if kind == 4 {
                        seq += 1; confirmed += 1;
                        Downstream::Committed(jade_collab_sync::protocol::Committed { seq, origin: ReplicaId(1), client_seq, base_seq, op: true })
                    } else { Downstream::Rejected(jade_collab_sync::protocol::Rejected { client_seq, reason: Rejection::new("refused") }) };
                    client.shared.replica.lock().unwrap().receive(message).unwrap();
                },
                _ => { let id = client.try_reserve(1).unwrap(); prop_assert_eq!(id.lamport, clock + 1); clock += 1; }
            }
            for (ticket, base_seq) in published {
                let message: ClientMessage = serde_json::from_str(&outbound.try_recv().unwrap()).unwrap();
                let ClientMessage::Submit(submission) = message else { panic!("expected a submission") };
                prop_assert_eq!((submission.client_seq, submission.base_seq), (ticket, base_seq));
            }
            prop_assert!(outbound.try_recv().is_err());
            let replica = client.shared.replica.lock().unwrap();
            prop_assert_eq!(replica.view().0, confirmed + pending.len() as u64);
            prop_assert_eq!(replica.pending_len(), pending.len());
            prop_assert_eq!(replica.seq(), seq);
            prop_assert_eq!(replica.next_client_seq(), ticket);
            prop_assert_eq!(replica.clone().try_reserve(1).unwrap().lamport, clock + 1);
        }
    }
}
