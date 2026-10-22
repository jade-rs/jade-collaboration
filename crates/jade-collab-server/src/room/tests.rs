//! Drive the real document actor with an independent append-only model.
use super::*;
use crate::outbox::outbox;
use crate::storage::Fsync;
use jade_collab_sync::{ApplyContext, Lane, Rejection};
use proptest::prelude::*;
use serde::Deserialize;

#[tokio::test(flavor = "current_thread")]
async fn a_saturated_document_inbox_must_wait_without_blocking_the_runtime() {
    let (commands, _inbox) = mailbox::channel(1, 16);
    let (reply, _answer) = oneshot::channel();
    commands.send(Command::Seq { reply }).await.unwrap();
    let (reply, _answer) = oneshot::channel();
    let waiting = commands.send(Command::Seq { reply });
    tokio::pin!(waiting);
    assert!(
        futures_util::poll!(&mut waiting).is_pending(),
        "the second command must wait for this one-slot inbox"
    );
    tokio::task::yield_now().await;
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Model(Vec<u64>);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Op {
    Add { value: u64, derived: u8 },
    Derived(u64),
    Refuse,
}

impl Document for Model {
    type Op = Op;
    type Effect = ();
    fn lane(_: &Op) -> Lane {
        Lane::Sequenced
    }
    fn max_lamport(&self) -> u64 {
        0
    }
    fn apply(&mut self, op: &Op, _: &ApplyContext) -> Result<(), Rejection> {
        match op {
            Op::Add { value, .. } | Op::Derived(value) => self.0.push(*value),
            Op::Refuse => return Err(Rejection::new("refused")),
        }
        Ok(())
    }
}

#[derive(Clone)]
struct FileModel(Model);
impl Materializer<Model> for FileModel {
    fn may_derive(&self, _: &Model, cause: &Op) -> bool {
        matches!(cause, Op::Add { derived, .. } if *derived > 0)
    }
    fn fork(&self, _: &Model) -> Result<Self, String> {
        Ok(self.clone())
    }

    fn materialize(&mut self, document: &Model, _: (), _: &ApplyContext) -> Result<(), String> {
        self.0 = document.clone();
        Ok(())
    }
    fn derived(&mut self, _: &Model, cause: &Op) -> Result<Vec<Op>, Rejection> {
        Ok(match cause {
            Op::Add { value, derived } => (1..=u64::from(*derived))
                .map(|n| Op::Derived(value + n))
                .collect(),
            _ => Vec::new(),
        })
    }
}

struct FormatModel;
impl Hosted for FormatModel {
    type Doc = Model;
    type Op = Op;
    type Mat = FileModel;
    const FORMAT: Format = Format::Docx;
    fn load(bytes: &[u8], _: &Model) -> Result<FileModel, String> {
        serde_json::from_slice(bytes)
            .map(FileModel)
            .map_err(|e| e.to_string())
    }
    fn open(bytes: &[u8]) -> Result<(Model, FileModel), String> {
        let file = Self::load(bytes, &Model::default())?;
        Ok((file.0.clone(), file))
    }
    fn export(document: &Model, file: &FileModel) -> Result<Vec<u8>, String> {
        assert_eq!(document, &file.0, "model and materialized file agree");
        Self::checkpoint_file(file)
    }
    fn checkpoint_file(file: &FileModel) -> Result<Vec<u8>, String> {
        serde_json::to_vec(&file.0).map_err(|e| e.to_string())
    }
    fn inspect(document: &Model, file: &FileModel) -> Result<Value, String> {
        assert_eq!(document, &file.0);
        Ok(serde_json::json!({"count": document.0.len()}))
    }
}

struct Actor {
    commands: Commands,
    thread: thread::JoinHandle<Result<(), String>>,
}
impl Actor {
    fn start(source: Source) -> Self {
        let (commands, inbox) = mailbox::channel(
            mailbox::DEFAULT_INCOMING_LIMIT,
            mailbox::DEFAULT_INCOMING_BYTES,
        );
        let (ready, opened) = mpsc::channel();
        let thread = thread::spawn(move || {
            let hosting = Hosting::<FormatModel>::open(source);
            ready
                .send(hosting.as_ref().map(|_| ()).map_err(Clone::clone))
                .unwrap();
            hosting?.serve(inbox)
        });
        opened.recv().unwrap().unwrap();
        Self { commands, thread }
    }
    fn send(&self, command: Command) {
        self.commands.blocking_send(command).unwrap();
    }
    fn export(&self) -> Model {
        let (reply, receive) = oneshot::channel();
        self.send(Command::Export { reply });
        serde_json::from_slice(&receive.blocking_recv().unwrap().unwrap()).unwrap()
    }
    fn stop(self) {
        drop(self.commands);
        self.thread.join().unwrap().unwrap();
    }
}

#[test]
fn exhausted_joins_are_refused_without_stopping_the_restored_actor_or_rebinding_owners() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("exhausted.docx");
    let options = StorageOptions::default();
    let mut checkpoint =
        Sequencer::with_materializer(Model::default(), FileModel(Model::default())).checkpoint();
    checkpoint.seq = u64::MAX;
    checkpoint.next_replica = u64::MAX;
    checkpoint.processed.insert(ReplicaId(1), 7);
    checkpoint.owners.insert(ReplicaId(1), "existing".into());
    drop(DocumentStore::create(dir.clone(), Format::Docx, options, &checkpoint, b"[]").unwrap());
    for _ in 0..2 {
        let actor = Actor::start(Source::Stored {
            dir: dir.clone(),
            options,
        });
        let (existing, mut existing_queue, _) = outbox(16);
        let (reply, received) = oneshot::channel();
        actor.send(Command::Resume {
            principal: Some("existing".into()),
            replica: ReplicaId(1),
            seq: u64::MAX,
            outbox: existing.clone(),
            reply,
        });
        received.blocking_recv().unwrap().unwrap();
        assert!(matches!(
            serde_json::from_str::<ServerMessage>(&existing_queue.blocking_recv().unwrap())
                .unwrap(),
            ServerMessage::Resumed {
                processed_client_seq: 7,
                ..
            }
        ));
        let (newcomer, mut newcomer_queue, _) = outbox(16);
        let (reply, joined) = oneshot::channel();
        actor.send(Command::Join {
            principal: Some("newcomer".into()),
            outbox: newcomer,
            reply,
        });
        assert_eq!(
            joined.blocking_recv().unwrap(),
            Err(SyncError::ReplicaIdExhausted)
        );
        assert!(
            newcomer_queue.try_recv().is_err(),
            "no welcome is published"
        );
        actor.send(Command::Inspect {
            replica: ReplicaId(1),
            outbox: existing,
            request: 1,
        });
        assert!(matches!(
            serde_json::from_str::<ServerMessage>(&existing_queue.blocking_recv().unwrap())
                .unwrap(),
            ServerMessage::Inspected { seq: u64::MAX, .. }
        ));
        assert_eq!(actor.export(), Model::default());
        actor.stop();
        let (_, stored) =
            DocumentStore::open::<Model, Op>(dir.clone(), Format::Docx, options).unwrap();
        assert_eq!(stored.checkpoint, checkpoint);
        assert!(stored.journal.is_empty());
    }
}

#[test]
fn a_replaced_connection_cannot_consume_the_resumed_connections_submission() {
    let actor = Actor::start(Source::File(b"[]".to_vec()));
    let (old, mut old_queue, _) = outbox(16);
    let (current, mut current_queue, _) = outbox(16);
    let (reply, joined) = oneshot::channel();
    actor.send(Command::Join {
        principal: None,
        outbox: old.clone(),
        reply,
    });
    let replica = joined.blocking_recv().unwrap().unwrap();
    old_queue.try_recv().unwrap();
    let (reply, resumed) = oneshot::channel();
    actor.send(Command::Resume {
        principal: None,
        replica,
        seq: 0,
        outbox: current.clone(),
        reply,
    });
    resumed.blocking_recv().unwrap().unwrap();
    current_queue.try_recv().unwrap();
    for (outbox, value) in [(old, 100), (current, 1)] {
        actor.send(Command::Submit {
            replica,
            outbox,
            author: None,
            refusal: None,
            submission: Submission {
                client_seq: 1,
                base_seq: 0,
                op: serde_json::to_value(Op::Add { value, derived: 1 }).unwrap(),
            },
        });
    }
    assert_eq!(actor.export(), Model(vec![1, 2]));
    let error: Value = serde_json::from_str(&old_queue.try_recv().unwrap()).unwrap();
    assert_eq!(error["type"], "error");
    for _ in 0..2 {
        let commit: Value = serde_json::from_str(&current_queue.try_recv().unwrap()).unwrap();
        assert_eq!(commit["type"], "committed");
    }
    assert!(current_queue.try_recv().is_err());
    actor.stop();
}

#[test]
fn only_the_current_connection_can_inspect_a_replica() {
    let actor = Actor::start(Source::File(b"[]".to_vec()));
    let (old, mut old_queue, _) = outbox(16);
    let (current, mut current_queue, _) = outbox(16);
    let (reply, joined) = oneshot::channel();
    actor.send(Command::Join {
        principal: None,
        outbox: old.clone(),
        reply,
    });
    let replica = joined.blocking_recv().unwrap().unwrap();
    old_queue.try_recv().unwrap();
    let (reply, resumed) = oneshot::channel();
    actor.send(Command::Resume {
        principal: None,
        replica,
        seq: 0,
        outbox: current.clone(),
        reply,
    });
    resumed.blocking_recv().unwrap().unwrap();
    current_queue.try_recv().unwrap();
    actor.send(Command::Inspect {
        replica,
        outbox: old,
        request: 11,
    });
    actor.send(Command::Inspect {
        replica,
        outbox: current,
        request: 12,
    });
    assert_eq!(actor.export(), Model::default());
    let denied: ServerMessage = serde_json::from_str(&old_queue.try_recv().unwrap()).unwrap();
    assert!(matches!(
        denied,
        ServerMessage::InspectionFailed { request: 11, .. }
    ));
    let accepted: ServerMessage = serde_json::from_str(&current_queue.try_recv().unwrap()).unwrap();
    assert!(matches!(
        accepted,
        ServerMessage::Inspected {
            request: 12,
            seq: 0,
            ..
        }
    ));
    assert!(old_queue.try_recv().is_err() && current_queue.try_recv().is_err());
    actor.stop();
}

/// The disk can fail after a partial append or after a full append at fsync.
/// Neither case may publish the cause before its derived records are durable.
#[test]
fn a_failed_batch_is_never_acknowledged_or_checkpointed() {
    use crate::storage::{JournalFile, JournalOpener};
    use std::fs::File;
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    struct Failing {
        file: File,
        writes: u8,
        fail_sync: bool,
    }
    impl JournalFile for Failing {
        fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.writes += 1;
            if self.writes == 2 && !self.fail_sync {
                self.file.write_all(&bytes[..bytes.len() / 2])?;
                return Err(io::Error::other("injected append failure"));
            }
            self.file.write_all(bytes)
        }
        fn sync(&mut self) -> io::Result<()> {
            if self.writes == 2 && self.fail_sync {
                return Err(io::Error::other("injected fsync failure"));
            }
            self.file.sync_data()
        }
    }

    for fail_sync in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("failure.docx");
        let options = StorageOptions {
            checkpoint_every: 2,
            sync: Fsync::EveryRecord,
            ..StorageOptions::default()
        };
        let sequencer = Sequencer::with_materializer(Model::default(), FileModel(Model::default()));
        let opens = Arc::new(Mutex::new(0));
        let count = opens.clone();
        let opener: JournalOpener = Arc::new(move |file| {
            *count.lock().unwrap() += 1;
            Box::new(Failing {
                file,
                writes: 0,
                fail_sync,
            })
        });
        let store = DocumentStore::create_with(
            dir.clone(),
            Format::Docx,
            options,
            &sequencer.checkpoint(),
            b"[]",
            opener,
        )
        .unwrap();
        let mut hosting = Hosting::<FormatModel> {
            sequencer,
            store: Some(store),
        };
        let (commands, inbox) = mailbox::channel(
            mailbox::DEFAULT_INCOMING_LIMIT,
            mailbox::DEFAULT_INCOMING_BYTES,
        );
        let (outbox, mut queue, _) = outbox(16);
        let (reply, _) = oneshot::channel();
        commands
            .blocking_send(Command::Join {
                principal: None,
                outbox: outbox.clone(),
                reply,
            })
            .unwrap();
        commands
            .blocking_send(Command::Submit {
                replica: ReplicaId(1),
                outbox: outbox.clone(),
                author: None,
                refusal: None,
                submission: Submission {
                    client_seq: 1,
                    base_seq: 0,
                    op: serde_json::to_value(Op::Add {
                        value: 10,
                        derived: 2,
                    })
                    .unwrap(),
                },
            })
            .unwrap();
        drop(commands);
        assert!(hosting.serve(inbox).unwrap_err().contains("injected"));
        let welcome: Value = serde_json::from_str(&queue.try_recv().unwrap()).unwrap();
        assert_eq!(welcome["type"], "welcome");
        assert!(
            queue.try_recv().is_err(),
            "no part of the failed batch was broadcast"
        );
        assert_eq!(*opens.lock().unwrap(), 1, "no checkpoint was attempted");
        drop(hosting);
        let restored = Hosting::<FormatModel>::open(Source::Stored { dir, options }).unwrap();
        let expected = if fail_sync { vec![10, 11, 12] } else { vec![] };
        assert_eq!(restored.sequencer.document().0, expected);
        assert_eq!(restored.sequencer.materializer().0 .0, expected);
    }
}

#[test]
fn a_named_join_is_not_published_before_its_ownership_batch_is_durable() {
    use crate::storage::{JournalFile, JournalOpener};
    use std::io::{self, Write};
    use std::sync::Arc;
    struct Failure {
        file: std::fs::File,
        fail_sync: bool,
    }
    impl JournalFile for Failure {
        fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
            if self.fail_sync {
                self.file.write_all(bytes)
            } else {
                self.file.write_all(&bytes[..bytes.len() / 2])?;
                Err(io::Error::other("join append failed"))
            }
        }
        fn sync(&mut self) -> io::Result<()> {
            Err(io::Error::other("join sync failed"))
        }
    }
    for fail_sync in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("owner.docx");
        let options = StorageOptions {
            checkpoint_every: 2,
            sync: Fsync::EveryRecord,
            ..StorageOptions::default()
        };
        let sequencer = Sequencer::with_materializer(Model::default(), FileModel(Model::default()));
        let opener: JournalOpener = Arc::new(move |file| Box::new(Failure { file, fail_sync }));
        let store = DocumentStore::create_with(
            dir.clone(),
            Format::Docx,
            options,
            &sequencer.checkpoint(),
            b"[]",
            opener,
        )
        .unwrap();
        let mut hosting = Hosting::<FormatModel> {
            sequencer,
            store: Some(store),
        };
        let (commands, inbox) = mailbox::channel(
            mailbox::DEFAULT_INCOMING_LIMIT,
            mailbox::DEFAULT_INCOMING_BYTES,
        );
        let (outbox, mut queue, _) = outbox(16);
        let (reply, _) = oneshot::channel();
        commands
            .blocking_send(Command::Join {
                principal: Some("主体".into()),
                outbox,
                reply,
            })
            .unwrap();
        drop(commands);
        assert!(hosting.serve(inbox).is_err());
        assert!(
            queue.try_recv().is_err(),
            "a welcome cannot precede durable ownership"
        );
        drop(hosting);
        let restored = Hosting::<FormatModel>::open(Source::Stored { dir, options }).unwrap();
        let replica = ReplicaId(1);
        assert_eq!(
            restored
                .sequencer
                .resume_as(replica, 0, Some("主体"))
                .is_ok(),
            fail_sync
        );
        assert!(restored.sequencer.resume_as(replica, 0, None).is_err());
        assert!(restored
            .sequencer
            .resume_as(replica, 0, Some("other"))
            .is_err());
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Submit, derive, reject, resend, checkpoint, restart and replace sockets, checking
    /// the real actor's file after *every* transition against an ordinary list.
    #[test]
    fn derived_edits_survive_checkpoint_restart_and_retry(
        checkpoint_every in 1..9u64,
        keep_tail in 0..8usize,
        every_record in any::<bool>(),
        named in any::<bool>(),
        steps in prop::collection::vec((0u8..9, 0..1000u64, 0u8..4), 1..45),
    ) {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("model.docx");
        let options = StorageOptions {
            checkpoint_every, keep_tail,
            sync: if every_record { Fsync::EveryRecord } else { Fsync::Checkpoints },
        };
        let mut actor = Actor::start(Source::NewStored {
            bytes: b"[]".to_vec(), dir: dir.clone(), options,
        });
        let (mut outbox, mut queue, _) = outbox(1024);
        let mut replaced = Vec::new();
        let (reply, joined) = oneshot::channel();
        let principal = named.then(|| "Ada".to_string());
        actor.send(Command::Join { principal: principal.clone(), outbox: outbox.clone(), reply });
        let replica = joined.blocking_recv().unwrap().unwrap();
        let mut expected = Model::default();
        let mut client_seq = 0;
        let mut seq = 0;
        let mut last = None;
        for (kind, value, derived) in steps {
            match kind {
                0..=3 => {
                    client_seq += 1;
                    let op = if kind == 3 && derived == 0 { Op::Refuse } else { Op::Add { value, derived } };
                    let refusal = (kind == 3 && derived != 0).then(|| jade_collab_sync::Rejection::new("read-only"));
                    let submission = Submission {
                        client_seq, base_seq: seq, op: serde_json::to_value(op).unwrap(),
                    };
                    actor.send(Command::Submit { replica, outbox: outbox.clone(), author: None, refusal, submission: submission.clone() });
                    last = Some(submission);
                    if kind != 3 {
                        expected.0.extend(value..=value + u64::from(derived));
                        seq += 1 + u64::from(derived);
                    }
                }
                4 => if let Some(submission) = last.clone() {
                    actor.send(Command::Submit { replica, outbox: outbox.clone(), author: None, refusal: None, submission });
                },
                5 => {
                    actor.stop();
                    actor = Actor::start(Source::Stored { dir: dir.clone(), options });
                    let (reply, resumed) = oneshot::channel();
                    actor.send(Command::Resume { principal: principal.clone(), replica, seq, outbox: outbox.clone(), reply });
                    resumed.blocking_recv().unwrap().unwrap();
                }
                6 => {
                    let (next, next_queue, _) = crate::outbox::outbox(1024);
                    let (reply, resumed) = oneshot::channel();
                    actor.send(Command::Resume { principal: principal.clone(), replica, seq, outbox: next.clone(), reply });
                    resumed.blocking_recv().unwrap().unwrap();
                    replaced.push((outbox, queue));
                    outbox = next;
                    queue = next_queue;
                }
                8 => {
                    let (reply, resumed) = oneshot::channel();
                    actor.send(Command::Resume { principal: Some("Mallory".into()), replica, seq, outbox: outbox.clone(), reply });
                    prop_assert!(resumed.blocking_recv().unwrap().is_err());
                }
                _ => if !replaced.is_empty() {
                    let (old, _) = &replaced[value as usize % replaced.len()];
                    actor.send(Command::Submit {
                        replica, outbox: old.clone(), author: None,
                        refusal: (derived != 0).then(|| jade_collab_sync::Rejection::new("stale read-only socket")),
                        submission: Submission {
                            client_seq: client_seq + 1, base_seq: seq,
                            op: serde_json::to_value(Op::Add { value: 10_000 + value, derived }).unwrap(),
                        },
                    });
                    actor.send(Command::Disconnected { replica: Some(replica), outbox: old.clone() });
                },
            }
            prop_assert_eq!(actor.export(), expected.clone());
            // Export is a barrier after all commits in this transition.
            while queue.try_recv().is_ok() {}
        }
        actor.stop();
        let restored = Hosting::<FormatModel>::open(Source::Stored { dir, options }).unwrap();
        prop_assert_eq!(restored.sequencer.document(), &expected);
        prop_assert_eq!(restored.sequencer.seq(), seq);
        prop_assert_eq!(restored.sequencer.processed_client_seq(replica), client_seq);
    }
}

#[test]
fn cancelled_join_cleanup_removes_the_assigned_connection_without_requiring_its_reply() {
    let actor = Actor::start(Source::File(b"[]".to_vec()));
    let (outbox, mut queue, _) = outbox(16);
    let (reply, answer) = oneshot::channel();
    drop(answer);
    actor.send(Command::Join {
        principal: None,
        outbox: outbox.clone(),
        reply,
    });
    actor.send(Command::Disconnected {
        replica: None,
        outbox: outbox.clone(),
    });
    actor.send(Command::Submit {
        replica: ReplicaId(1),
        outbox,
        author: None,
        refusal: None,
        submission: Submission {
            client_seq: 1,
            base_seq: 0,
            op: serde_json::to_value(Op::Add {
                value: 1,
                derived: 0,
            })
            .unwrap(),
        },
    });
    assert_eq!(actor.export(), Model::default());
    assert!(matches!(
        serde_json::from_str::<ServerMessage>(&queue.try_recv().unwrap()).unwrap(),
        ServerMessage::Welcome { .. }
    ));
    assert!(matches!(
        serde_json::from_str::<ServerMessage>(&queue.try_recv().unwrap()).unwrap(),
        ServerMessage::Error { .. }
    ));
    actor.stop();
}
