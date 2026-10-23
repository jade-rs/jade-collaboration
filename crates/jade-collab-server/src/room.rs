//! One document's actor thread.

#[cfg(test)]
mod author_tests;

#[cfg(test)]
pub(crate) mod failure_tests;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

use jade_collab_core::id::ReplicaId;
use jade_collab_sync::protocol::{Committed, Submission};
use jade_collab_sync::{
    ApplyContext, Document, Materializer, Record, Rejection, Sequencer, SubmitOutcome, SyncError,
};
use jade_docx_collab::{DocxModel, DocxOp, JadeDocx};
use jade_pptx_collab::{JadePptx, PptxModel};
use jade_xlsx_collab::{JadeXlsx, LoadOptions, XlsxModel};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::mailbox;
use crate::storage::{DocumentStore, StorageOptions};
use crate::wire::{Format, ServerMessage};

pub(crate) use crate::outbox::Outbox;

pub(crate) enum Command {
    Join {
        principal: Option<String>,
        outbox: Outbox,
        reply: oneshot::Sender<Result<ReplicaId, SyncError>>,
    },
    Resume {
        principal: Option<String>,
        replica: ReplicaId,
        seq: u64,
        outbox: Outbox,
        reply: oneshot::Sender<Result<(), SyncError>>,
    },
    Submit {
        replica: ReplicaId,
        /// Only the replica's current connection may submit on its behalf.
        outbox: Outbox,
        /// The connection's token name, which the server makes the author
        /// of the replica's tracked changes.
        author: Option<String>,
        /// Permission refusal is processed and journaled by the current actor
        /// before acknowledging the ticket, without inspecting the payload.
        refusal: Option<Rejection>,
        submission: Submission<Value>,
    },
    /// Carries the connection's outbox, so a late update from a replica's
    /// old connection does not override its newer one.
    Presence {
        replica: ReplicaId,
        name: Option<String>,
        state: Value,
        outbox: Outbox,
    },
    Export {
        reply: oneshot::Sender<Result<Vec<u8>, String>>,
    },
    Inspect {
        replica: ReplicaId,
        outbox: Outbox,
        request: u64,
    },
    Seq {
        reply: oneshot::Sender<u64>,
    },
    Faults {
        reply: oneshot::Sender<Vec<String>>,
    },
    /// Cleanup by connection identity, even when a cancelled join has not
    /// returned its assigned replica. A resumed connection must remain intact.
    Disconnected {
        replica: Option<ReplicaId>,
        outbox: Outbox,
    },
}

pub(crate) type Commands = mailbox::Sender<Command>;

/// Owns the document thread, even while sockets hold command senders.
/// Dropping its server stops and joins it before storage can change owners.
pub(crate) struct Actor {
    pub commands: Commands,
    thread: Option<thread::JoinHandle<()>>,
}

impl Drop for Actor {
    fn drop(&mut self) {
        self.commands.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
impl Actor {
    pub(super) fn test_worker(
        count: usize,
        bytes: usize,
        work: impl FnOnce(mailbox::Inbox<Command>) + Send + 'static,
    ) -> Self {
        let (commands, inbox) = mailbox::channel(count, bytes);
        Self {
            commands,
            thread: Some(thread::spawn(move || work(inbox))),
        }
    }
}

/// A format the server can host: how to open it and write it back.
trait Hosted: 'static {
    type Doc: Document<Op = Self::Op> + Serialize + DeserializeOwned;
    type Op: Clone + std::fmt::Debug + PartialEq + Serialize + DeserializeOwned;
    type Mat: Materializer<Self::Doc>;
    const FORMAT: Format;

    /// The real document only, for a model that is restored rather than
    /// derived from the file.
    fn load(bytes: &[u8], model: &Self::Doc) -> Result<Self::Mat, String>;
    /// The real document and the model derived from it.
    fn open(bytes: &[u8]) -> Result<(Self::Doc, Self::Mat), String>;
    /// The file as users get it.
    fn export(document: &Self::Doc, materializer: &Self::Mat) -> Result<Vec<u8>, String>;
    /// The file a checkpoint stores next to the model, which `load` reads
    /// back.
    fn checkpoint_file(materializer: &Self::Mat) -> Result<Vec<u8>, String>;

    fn inspect(_document: &Self::Doc, _materializer: &Self::Mat) -> Result<Value, String> {
        Err("native formatting inspection is currently available for DOCX and PPTX only".into())
    }

    /// The operation the server commits, as the document's origin, before
    /// `op` from `replica` — whose token is named `author` — so that the
    /// revision `op` makes carries that name; `None` when nothing needs
    /// saying (the default).
    fn author_op(
        _document: &Self::Doc,
        _replica: ReplicaId,
        _author: &str,
        _op: &Self::Op,
    ) -> Option<Self::Op> {
        None
    }
}

struct Docx;

impl Hosted for Docx {
    type Doc = DocxModel;
    type Op = jade_docx_collab::DocxOp;
    type Mat = JadeDocx;
    const FORMAT: Format = Format::Docx;

    fn load(bytes: &[u8], model: &DocxModel) -> Result<JadeDocx, String> {
        JadeDocx::restore(bytes, model)
    }

    fn open(bytes: &[u8]) -> Result<(DocxModel, JadeDocx), String> {
        let jade = JadeDocx::load(bytes)?;
        let model = DocxModel::from_word_document(jade.word()).map_err(|e| e.to_string())?;
        Ok((model, jade))
    }

    fn export(model: &DocxModel, materializer: &JadeDocx) -> Result<Vec<u8>, String> {
        materializer.export(model)
    }

    fn inspect(model: &DocxModel, materializer: &JadeDocx) -> Result<Value, String> {
        serde_json::to_value(materializer.inspect_formatting(model)?).map_err(|e| e.to_string())
    }

    /// The base file: the model's marks are in the checkpoint's model.
    fn checkpoint_file(materializer: &JadeDocx) -> Result<Vec<u8>, String> {
        materializer.export_base().map_err(|e| e.to_string())
    }

    /// A tracked change by a replica the model does not know under its
    /// token's name is preceded by `SetAuthor`.
    fn author_op(
        document: &DocxModel,
        replica: ReplicaId,
        author: &str,
        op: &DocxOp,
    ) -> Option<DocxOp> {
        let tracked = matches!(
            op,
            DocxOp::TrackInsertText { .. } | DocxOp::TrackDeleteText { .. }
        );
        (tracked && document.author(replica) != Some(author)).then(|| DocxOp::SetAuthor {
            replica,
            name: author.to_string(),
        })
    }
}

struct Xlsx;

impl Hosted for Xlsx {
    type Doc = XlsxModel;
    type Op = jade_xlsx_collab::XlsxOp;
    type Mat = JadeXlsx;
    const FORMAT: Format = Format::Xlsx;

    fn load(bytes: &[u8], _: &XlsxModel) -> Result<JadeXlsx, String> {
        JadeXlsx::open(bytes)
    }

    fn open(bytes: &[u8]) -> Result<(XlsxModel, JadeXlsx), String> {
        let jade = JadeXlsx::open(bytes)?;
        let model = XlsxModel::from_workbook(jade.workbook(), LoadOptions::default())
            .map_err(|e| e.to_string())?;
        Ok((model, jade))
    }

    fn export(_: &XlsxModel, materializer: &JadeXlsx) -> Result<Vec<u8>, String> {
        materializer.export()
    }

    fn checkpoint_file(materializer: &JadeXlsx) -> Result<Vec<u8>, String> {
        materializer.export()
    }
}

struct Pptx;

impl Hosted for Pptx {
    type Doc = PptxModel;
    type Op = jade_pptx_collab::PptxOp;
    type Mat = JadePptx;
    const FORMAT: Format = Format::Pptx;

    /// The materializer remembers which file id jade gave each text box made
    /// in the session; restoring rebuilds that from the model.
    fn load(bytes: &[u8], model: &PptxModel) -> Result<JadePptx, String> {
        JadePptx::restore(bytes, model)
    }

    fn open(bytes: &[u8]) -> Result<(PptxModel, JadePptx), String> {
        let jade = JadePptx::open(bytes)?;
        let model = PptxModel::from_pptx(jade.bytes()).map_err(|e| e.to_string())?;
        Ok((model, jade))
    }

    fn export(model: &PptxModel, materializer: &JadePptx) -> Result<Vec<u8>, String> {
        materializer.export(model)
    }

    fn inspect(model: &PptxModel, materializer: &JadePptx) -> Result<Value, String> {
        serde_json::to_value(materializer.inspect_formatting(model)?).map_err(|e| e.to_string())
    }

    /// The base file: the model's marks are in the checkpoint's model.
    fn checkpoint_file(materializer: &JadePptx) -> Result<Vec<u8>, String> {
        Ok(materializer.export_base())
    }
}

/// Where a document comes from.
pub(crate) enum Source {
    /// A file, kept in memory only.
    File(Vec<u8>),
    /// A file, made durable in a new directory.
    NewStored {
        bytes: Vec<u8>,
        dir: PathBuf,
        options: StorageOptions,
    },
    /// A directory a durable server wrote.
    Stored {
        dir: PathBuf,
        options: StorageOptions,
    },
}

/// Starts a document thread; returns once the document has opened (or failed to).
pub(crate) fn spawn(name: &str, format: Format, source: Source) -> Result<Actor, String> {
    let (commands, inbox) = mailbox::channel(
        mailbox::DEFAULT_INCOMING_LIMIT,
        mailbox::DEFAULT_INCOMING_BYTES,
    );
    let (ready, opened) = mpsc::channel();
    let thread = thread::Builder::new()
        .name(format!("doc:{name}"))
        .spawn(move || match format {
            Format::Docx => host::<Docx>(source, inbox, ready),
            Format::Xlsx => host::<Xlsx>(source, inbox, ready),
            Format::Pptx => host::<Pptx>(source, inbox, ready),
        })
        .map_err(|e| e.to_string())?;
    let actor = Actor {
        commands,
        thread: Some(thread),
    };
    opened.recv().map_err(|e| e.to_string())??;
    Ok(actor)
}

fn host<H: Hosted>(
    source: Source,
    inbox: mailbox::Inbox<Command>,
    ready: mpsc::Sender<Result<(), String>>,
) {
    let mut hosting = match Hosting::<H>::open(source) {
        Ok(hosting) => {
            let _ = ready.send(Ok(()));
            hosting
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    if let Err(error) = hosting.serve(inbox) {
        // Answering without a durable journal would let replicas run ahead
        // of the disk; the document stops instead.
        eprintln!("document stopped: {error}");
    }
}

fn committed_json<Op: Serialize>(committed: Committed<Op>) -> Committed<Value> {
    Committed {
        seq: committed.seq,
        origin: committed.origin,
        client_seq: committed.client_seq,
        base_seq: committed.base_seq,
        op: serde_json::to_value(&committed.op).expect("operations serialize"),
    }
}

/// A document and, on a durable server, its directory.
struct Hosting<H: Hosted> {
    sequencer: Sequencer<H::Doc, H::Mat>,
    store: Option<DocumentStore>,
}

impl<H: Hosted> Hosting<H> {
    /// Check the complete logical author/edit pair before publishing either.
    /// Only the first tracked edit (or a changed author) needs this clone;
    /// ordinary typing keeps the existing path without a native file fork.
    fn prepare_authored_edit(
        &self,
        named: &H::Op,
        replica: ReplicaId,
        base_seq: u64,
        op: &H::Op,
    ) -> Result<(), Rejection> {
        let current = self.sequencer.seq();
        let edit_seq = current
            .checked_add(2)
            .ok_or_else(|| Rejection::new(SyncError::LogExhausted.to_string()))?;
        let mut trial = self.sequencer.document().clone();
        trial.apply(
            named,
            &ApplyContext {
                origin: ReplicaId::ORIGIN,
                base_seq: current,
                seq: Some(edit_seq - 1),
            },
        )?;
        trial.apply(
            op,
            &ApplyContext {
                origin: replica,
                base_seq,
                seq: Some(edit_seq),
            },
        )?;
        Ok(())
    }

    fn open(source: Source) -> Result<Self, String> {
        Ok(match source {
            Source::File(bytes) => {
                let (document, materializer) = H::open(&bytes)?;
                Hosting {
                    sequencer: Sequencer::with_materializer(document, materializer),
                    store: None,
                }
            }
            Source::NewStored {
                bytes,
                dir,
                options,
            } => {
                let (document, materializer) = H::open(&bytes)?;
                let sequencer = Sequencer::with_materializer(document, materializer);
                let file = H::checkpoint_file(sequencer.materializer())?;
                let store =
                    DocumentStore::create(dir, H::FORMAT, options, &sequencer.checkpoint(), &file)?;
                Hosting {
                    sequencer,
                    store: Some(store),
                }
            }
            Source::Stored { dir, options } => {
                let (store, stored) =
                    DocumentStore::open::<H::Doc, H::Op>(dir, H::FORMAT, options)?;
                let materializer = H::load(&stored.file, &stored.checkpoint.document)?;
                let mut sequencer =
                    Sequencer::restore_with_tail(stored.checkpoint, stored.tail, materializer)
                        .map_err(|e| e.to_string())?;
                for (index, record) in stored.journal.into_iter().enumerate() {
                    sequencer
                        .replay(record)
                        .map_err(|e| format!("journal record {}: {e}", index + 1))?;
                }
                Hosting {
                    sequencer,
                    store: Some(store),
                }
            }
        })
    }

    /// Journals `record` before anyone hears of it, and checkpoints when due.
    fn journal(&mut self, record: Record<H::Op>) -> Result<(), String> {
        self.journal_batch(&[record])
    }

    /// The sequencer has already applied the whole batch. Persist it before
    /// checkpointing that state or publishing even its first operation.
    fn journal_batch(&mut self, records: &[Record<H::Op>]) -> Result<(), String> {
        let Some(store) = &mut self.store else {
            return Ok(());
        };
        if store.append_batch(records)? {
            // A file that cannot be exported now is checkpointed later; the
            // journal keeps everything until then.
            match H::checkpoint_file(self.sequencer.materializer()) {
                Ok(file) => {
                    let keep = store.keep_tail();
                    let log = self.sequencer.log();
                    let tail = &log[log.len().saturating_sub(keep)..];
                    store.checkpoint(&self.sequencer.checkpoint(), tail, &file)?;
                    self.sequencer.compact_keeping(keep);
                }
                Err(error) => eprintln!("cannot checkpoint: {error}"),
            }
        }
        Ok(())
    }

    fn serve(&mut self, inbox: mailbox::Inbox<Command>) -> Result<(), String> {
        let mut subscribers: BTreeMap<ReplicaId, Outbox> = BTreeMap::new();
        let result = self.serve_subscribers(inbox, &mut subscribers);
        // Connections hold their own outbox sender, so dropping the map
        // alone cannot close their outgoing queues. Wake them explicitly.
        for outbox in subscribers.values() {
            outbox.stop();
        }
        result
    }

    fn serve_subscribers(
        &mut self,
        mut inbox: mailbox::Inbox<Command>,
        subscribers: &mut BTreeMap<ReplicaId, Outbox>,
    ) -> Result<(), String> {
        // Ephemeral: in memory only, gone when the connection is.
        let mut presence: BTreeMap<ReplicaId, ServerMessage> = BTreeMap::new();
        // A subscriber that cannot take a message is gone or too far behind:
        // it is dropped, and its connection closes.
        let others = |subscribers: &mut BTreeMap<ReplicaId, Outbox>, replica, json: String| {
            subscribers
                .retain(|other, outbox| *other == replica || outbox.send(json.clone()).is_ok());
        };
        while let Some(envelope) = inbox.blocking_recv() {
            let mailbox::Envelope {
                value: command,
                _bytes,
            } = envelope;
            match command {
                Command::Join {
                    principal,
                    outbox,
                    reply,
                } => {
                    let welcome = match self.sequencer.try_join_as(principal.clone()) {
                        Ok(welcome) => welcome,
                        Err(error) => {
                            let _ = reply.send(Err(error));
                            continue;
                        }
                    };
                    let mut records = vec![Record::Joined {
                        replica: welcome.replica,
                    }];
                    if let Some(principal) = principal {
                        records.push(Record::Owned {
                            replica: welcome.replica,
                            principal,
                        });
                    }
                    self.journal_batch(&records)?;
                    let message = ServerMessage::Welcome {
                        format: H::FORMAT,
                        replica: welcome.replica,
                        seq: welcome.seq,
                        snapshot: serde_json::to_value(&welcome.snapshot)
                            .expect("models serialize"),
                    };
                    // Sent from this thread before any later commit, so the
                    // replica starts exactly at `seq`.
                    let _ = outbox.send(message.to_json());
                    for message in presence.values() {
                        let _ = outbox.send(message.to_json());
                    }
                    subscribers.insert(welcome.replica, outbox);
                    let _ = reply.send(Ok(welcome.replica));
                }
                Command::Resume {
                    principal,
                    replica,
                    seq,
                    outbox,
                    reply,
                } => match self.sequencer.resume_as(replica, seq, principal.as_deref()) {
                    Ok(resume) => {
                        let message = ServerMessage::Resumed {
                            processed_client_seq: resume.processed_client_seq,
                            missed: resume.missed.into_iter().map(committed_json).collect(),
                        };
                        let _ = outbox.send(message.to_json());
                        // Whatever the replica's old connection showed is stale.
                        if presence.remove(&replica).is_some() {
                            let left = ServerMessage::PresenceLeft { replica }.to_json();
                            others(subscribers, replica, left);
                        }
                        for message in presence.values() {
                            let _ = outbox.send(message.to_json());
                        }
                        subscribers.insert(replica, outbox);
                        let _ = reply.send(Ok(()));
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                },
                Command::Submit {
                    replica,
                    outbox,
                    author,
                    refusal,
                    submission,
                } => {
                    // A resume replaces the connection. Late frames from
                    // the old socket must not consume the new one's client
                    // sequence numbers or name its tracked-change author.
                    if !subscribers
                        .get(&replica)
                        .is_some_and(|current| current.same_channel(&outbox))
                    {
                        let _ = outbox.send(
                            ServerMessage::Error {
                                message: "this connection no longer owns the replica; resume again"
                                    .into(),
                            }
                            .to_json(),
                        );
                        continue;
                    }
                    // Deduplicate before decoding, admission or author
                    // registration: a changed payload cannot revive a ticket.
                    if submission.client_seq <= self.sequencer.processed_client_seq(replica) {
                        continue;
                    }
                    if let Some(reason) = refusal {
                        let outcome = self
                            .sequencer
                            .reject(replica, submission.client_seq, reason);
                        if let Some(record) = outcome.record(replica) {
                            self.journal(record)?;
                        }
                        if let SubmitOutcome::Rejected(rejected) = outcome {
                            let _ = outbox.send(ServerMessage::Rejected(rejected).to_json());
                        }
                        continue;
                    }
                    let op = match serde_json::from_value::<H::Op>(submission.op) {
                        Ok(op) => op,
                        Err(error) => {
                            if let Some(outbox) = subscribers.get(&replica) {
                                let message = ServerMessage::Error {
                                    message: format!(
                                        "submission {} is not a valid operation: {error}",
                                        submission.client_seq
                                    ),
                                };
                                let _ = outbox.send(message.to_json());
                            }
                            continue;
                        }
                    };
                    // Refuse forged allocation namespaces before even author
                    // registration or native admission can create side effects.
                    if let Err(reason) = self.sequencer.document().validate_submission(&op, replica)
                    {
                        let outcome = self
                            .sequencer
                            .reject(replica, submission.client_seq, reason);
                        if let Some(record) = outcome.record(replica) {
                            self.journal(record)?;
                        }
                        if let SubmitOutcome::Rejected(rejected) = outcome {
                            let _ = outbox.send(ServerMessage::Rejected(rejected).to_json());
                        }
                        continue;
                    }
                    // Who the replica is goes into the log before what it
                    // does, for every replica to apply in that order.
                    let named = author.as_deref().and_then(|author| {
                        H::author_op(self.sequencer.document(), replica, author, &op)
                    });
                    if let Some(named) = named {
                        let prepared =
                            self.prepare_authored_edit(&named, replica, submission.base_seq, &op);
                        match prepared.and_then(|()| self.sequencer.commit_origin(named)) {
                            Ok(committed) => {
                                self.journal(Record::Committed(committed.clone()))?;
                                let json =
                                    ServerMessage::Committed(committed_json(committed)).to_json();
                                subscribers.retain(|_, outbox| outbox.send(json.clone()).is_ok());
                            }
                            Err(error) => {
                                // Native errors are unknown outcomes and stop
                                // this room. Logical refusal of either the
                                // author prerequisite or its edit changes only
                                // the processed ticket.
                                self.sequencer.check_health().map_err(|e| e.to_string())?;
                                let outcome = self.sequencer.reject(
                                    replica,
                                    submission.client_seq,
                                    Rejection::new(format!(
                                        "cannot prepare authenticated tracked edit: {error}"
                                    )),
                                );
                                if let Some(record) = outcome.record(replica) {
                                    self.journal(record)?;
                                }
                                if let SubmitOutcome::Rejected(rejected) = outcome {
                                    let _ =
                                        outbox.send(ServerMessage::Rejected(rejected).to_json());
                                }
                                continue;
                            }
                        }
                    }
                    let submission = Submission {
                        client_seq: submission.client_seq,
                        base_seq: submission.base_seq,
                        op,
                    };
                    let outcome = self.sequencer.submit(replica, submission);
                    // The file/model may contain partial native writes. Stop
                    // before journaling, checkpointing or publishing a verdict.
                    if let SubmitOutcome::Failed(error) = &outcome {
                        return Err(error.to_string());
                    }
                    let derived = self.sequencer.take_derived();
                    let mut records: Vec<_> = outcome.record(replica).into_iter().collect();
                    records.extend(derived.iter().cloned().map(Record::Committed));
                    if !records.is_empty() {
                        self.journal_batch(&records)?;
                    }
                    match outcome {
                        SubmitOutcome::Committed(committed) => {
                            let json =
                                ServerMessage::Committed(committed_json(committed)).to_json();
                            subscribers.retain(|_, outbox| outbox.send(json.clone()).is_ok());
                            // What jade did on its own, right after its cause.
                            for derived in derived {
                                let json =
                                    ServerMessage::Committed(committed_json(derived)).to_json();
                                subscribers.retain(|_, outbox| outbox.send(json.clone()).is_ok());
                            }
                        }
                        SubmitOutcome::Rejected(rejected) => {
                            if let Some(outbox) = subscribers.get(&replica) {
                                let _ = outbox.send(ServerMessage::Rejected(rejected).to_json());
                            }
                        }
                        SubmitOutcome::Duplicate => {}
                        SubmitOutcome::Failed(error) => return Err(error.to_string()),
                    }
                }
                Command::Presence {
                    replica,
                    name,
                    state,
                    outbox,
                } => {
                    if subscribers
                        .get(&replica)
                        .is_some_and(|current| current.same_channel(&outbox))
                    {
                        let message = ServerMessage::Presence {
                            replica,
                            name,
                            state,
                        };
                        others(subscribers, replica, message.to_json());
                        presence.insert(replica, message);
                    }
                }
                Command::Export { reply } => {
                    let _ = reply.send(H::export(
                        self.sequencer.document(),
                        self.sequencer.materializer(),
                    ));
                }
                Command::Inspect {
                    replica,
                    outbox,
                    request,
                } => {
                    if !subscribers
                        .get(&replica)
                        .is_some_and(|current| current.same_channel(&outbox))
                    {
                        let _ = outbox.send(
                            ServerMessage::InspectionFailed {
                                request,
                                message: "this replica's connection was superseded".into(),
                            }
                            .to_json(),
                        );
                        continue;
                    }
                    let response = match H::inspect(
                        self.sequencer.document(),
                        self.sequencer.materializer(),
                    ) {
                        Ok(formatting) => ServerMessage::Inspected {
                            request,
                            seq: self.sequencer.seq(),
                            snapshot: serde_json::to_value(self.sequencer.document())
                                .expect("models serialize"),
                            formatting,
                        },
                        Err(message) => ServerMessage::InspectionFailed { request, message },
                    };
                    let _ = outbox.send(response.to_json());
                }
                Command::Seq { reply } => {
                    let _ = reply.send(self.sequencer.seq());
                }
                Command::Faults { reply } => {
                    let _ = reply.send(self.sequencer.faults().to_vec());
                }
                Command::Disconnected { replica, outbox } => {
                    let mut departed: Vec<_> = subscribers
                        .iter()
                        .filter_map(|(id, current)| current.same_channel(&outbox).then_some(*id))
                        .collect();
                    if let Some(replica) = replica.filter(|id| !subscribers.contains_key(id)) {
                        departed.push(replica);
                    }
                    for replica in departed {
                        subscribers.remove(&replica);
                        if presence.remove(&replica).is_some() {
                            others(
                                subscribers,
                                replica,
                                ServerMessage::PresenceLeft { replica }.to_json(),
                            );
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
