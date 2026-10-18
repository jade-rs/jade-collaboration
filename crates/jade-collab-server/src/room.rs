//! One document's actor thread.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

use jade_collab_core::id::ReplicaId;
use jade_collab_sync::protocol::{Committed, Submission};
use jade_collab_sync::{Document, Materializer, Record, Sequencer, SubmitOutcome, SyncError};
use jade_docx_collab::{DocxModel, DocxOp, JadeDocx};
use jade_pptx_collab::{JadePptx, PptxModel};
use jade_xlsx_collab::{JadeXlsx, LoadOptions, XlsxModel};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{mpsc::UnboundedSender, oneshot};

use crate::storage::{DocumentStore, StorageOptions};
use crate::wire::{Format, ServerMessage};

pub(crate) type Outbox = UnboundedSender<String>;

pub(crate) enum Command {
    Join {
        outbox: Outbox,
        reply: oneshot::Sender<ReplicaId>,
    },
    Resume {
        replica: ReplicaId,
        seq: u64,
        outbox: Outbox,
        reply: oneshot::Sender<Result<(), SyncError>>,
    },
    Submit {
        replica: ReplicaId,
        /// The connection's token name, which the server makes the author
        /// of the replica's tracked changes.
        author: Option<String>,
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
    /// A connection closed. Carries its outbox so a late `Leave` from an old
    /// connection does not unsubscribe the replica's newer one.
    Leave {
        replica: ReplicaId,
        outbox: Outbox,
    },
    Export {
        reply: oneshot::Sender<Result<Vec<u8>, String>>,
    },
    Seq {
        reply: oneshot::Sender<u64>,
    },
    Faults {
        reply: oneshot::Sender<Vec<String>>,
    },
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
    fn export(materializer: &Self::Mat) -> Result<Vec<u8>, String>;

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

    fn load(bytes: &[u8], _: &DocxModel) -> Result<JadeDocx, String> {
        JadeDocx::load(bytes)
    }

    fn open(bytes: &[u8]) -> Result<(DocxModel, JadeDocx), String> {
        let jade = JadeDocx::load(bytes)?;
        let model = DocxModel::from_word_document(jade.word()).map_err(|e| e.to_string())?;
        Ok((model, jade))
    }

    fn export(materializer: &JadeDocx) -> Result<Vec<u8>, String> {
        materializer.export().map_err(|e| e.to_string())
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

    fn export(materializer: &JadeXlsx) -> Result<Vec<u8>, String> {
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

    fn export(materializer: &JadePptx) -> Result<Vec<u8>, String> {
        materializer.export()
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
pub(crate) fn spawn(
    name: &str,
    format: Format,
    source: Source,
) -> Result<mpsc::Sender<Command>, String> {
    let (commands, inbox) = mpsc::channel();
    let (ready, opened) = mpsc::channel();
    thread::Builder::new()
        .name(format!("doc:{name}"))
        .spawn(move || match format {
            Format::Docx => host::<Docx>(source, inbox, ready),
            Format::Xlsx => host::<Xlsx>(source, inbox, ready),
            Format::Pptx => host::<Pptx>(source, inbox, ready),
        })
        .map_err(|e| e.to_string())?;
    opened.recv().map_err(|e| e.to_string())??;
    Ok(commands)
}

fn host<H: Hosted>(
    source: Source,
    inbox: mpsc::Receiver<Command>,
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
                let file = H::export(sequencer.materializer())?;
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
        let Some(store) = &mut self.store else {
            return Ok(());
        };
        if store.append(&record)? {
            // A file that cannot be exported now is checkpointed later; the
            // journal keeps everything until then.
            match H::export(self.sequencer.materializer()) {
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

    fn serve(&mut self, inbox: mpsc::Receiver<Command>) -> Result<(), String> {
        let mut subscribers: BTreeMap<ReplicaId, Outbox> = BTreeMap::new();
        // Ephemeral: in memory only, gone when the connection is.
        let mut presence: BTreeMap<ReplicaId, ServerMessage> = BTreeMap::new();
        let others = |subscribers: &BTreeMap<ReplicaId, Outbox>, replica, json: String| {
            for (other, outbox) in subscribers {
                if *other != replica {
                    let _ = outbox.send(json.clone());
                }
            }
        };
        while let Ok(command) = inbox.recv() {
            match command {
                Command::Join { outbox, reply } => {
                    let welcome = self.sequencer.join();
                    self.journal(Record::Joined {
                        replica: welcome.replica,
                    })?;
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
                    let _ = reply.send(welcome.replica);
                }
                Command::Resume {
                    replica,
                    seq,
                    outbox,
                    reply,
                } => match self.sequencer.resume(replica, seq) {
                    Ok(resume) => {
                        let message = ServerMessage::Resumed {
                            processed_client_seq: resume.processed_client_seq,
                            missed: resume.missed.into_iter().map(committed_json).collect(),
                        };
                        let _ = outbox.send(message.to_json());
                        // Whatever the replica's old connection showed is stale.
                        if presence.remove(&replica).is_some() {
                            let left = ServerMessage::PresenceLeft { replica }.to_json();
                            others(&subscribers, replica, left);
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
                    author,
                    submission,
                } => {
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
                    // Who the replica is goes into the log before what it
                    // does, for every replica to apply in that order.
                    let named = author.as_deref().and_then(|author| {
                        H::author_op(self.sequencer.document(), replica, author, &op)
                    });
                    if let Some(named) = named {
                        match self.sequencer.commit_origin(named) {
                            Ok(committed) => {
                                self.journal(Record::Committed(committed.clone()))?;
                                let json =
                                    ServerMessage::Committed(committed_json(committed)).to_json();
                                subscribers.retain(|_, outbox| outbox.send(json.clone()).is_ok());
                            }
                            Err(error) => eprintln!("cannot name replica {replica}: {error}"),
                        }
                    }
                    let submission = Submission {
                        client_seq: submission.client_seq,
                        base_seq: submission.base_seq,
                        op,
                    };
                    let outcome = self.sequencer.submit(replica, submission);
                    if let Some(record) = outcome.record(replica) {
                        self.journal(record)?;
                    }
                    match outcome {
                        SubmitOutcome::Committed(committed) => {
                            let json =
                                ServerMessage::Committed(committed_json(committed)).to_json();
                            subscribers.retain(|_, outbox| outbox.send(json.clone()).is_ok());
                            // What jade did on its own, right after its cause.
                            for derived in self.sequencer.take_derived() {
                                self.journal(Record::Committed(derived.clone()))?;
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
                        others(&subscribers, replica, message.to_json());
                        presence.insert(replica, message);
                    }
                }
                Command::Leave { replica, outbox } => {
                    // A subscriber whose outbox failed was dropped already;
                    // its `Leave` still ends its presence.
                    let current = subscribers
                        .get(&replica)
                        .is_none_or(|current| current.same_channel(&outbox));
                    if current {
                        subscribers.remove(&replica);
                        presence.remove(&replica);
                        let left = ServerMessage::PresenceLeft { replica }.to_json();
                        others(&subscribers, replica, left);
                    }
                }
                Command::Export { reply } => {
                    let _ = reply.send(H::export(self.sequencer.materializer()));
                }
                Command::Seq { reply } => {
                    let _ = reply.send(self.sequencer.seq());
                }
                Command::Faults { reply } => {
                    let _ = reply.send(self.sequencer.faults().to_vec());
                }
            }
        }
        Ok(())
    }
}
