//! One document's actor thread.

use std::collections::BTreeMap;
use std::sync::mpsc;
use std::thread;

use jade_collab_core::id::ReplicaId;
use jade_collab_sync::protocol::{Committed, Submission};
use jade_collab_sync::{Document, Materializer, Sequencer, SubmitOutcome};
use jade_docx_collab::{DocxModel, JadeDocx};
use jade_xlsx_collab::{JadeXlsx, LoadOptions, XlsxModel};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{mpsc::UnboundedSender, oneshot};

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
        reply: oneshot::Sender<Result<(), String>>,
    },
    Submit {
        replica: ReplicaId,
        submission: Submission<Value>,
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
    type Doc: Document<Op = Self::Op> + Serialize;
    type Op: Clone + std::fmt::Debug + PartialEq + Serialize + DeserializeOwned;
    type Mat: Materializer<Self::Doc>;
    const FORMAT: Format;

    fn open(bytes: &[u8]) -> Result<(Self::Doc, Self::Mat), String>;
    fn export(materializer: &Self::Mat) -> Result<Vec<u8>, String>;
}

struct Docx;

impl Hosted for Docx {
    type Doc = DocxModel;
    type Op = jade_docx_collab::DocxOp;
    type Mat = JadeDocx;
    const FORMAT: Format = Format::Docx;

    fn open(bytes: &[u8]) -> Result<(DocxModel, JadeDocx), String> {
        let jade = JadeDocx::load(bytes)?;
        let model = DocxModel::from_word_document(jade.word()).map_err(|e| e.to_string())?;
        Ok((model, jade))
    }

    fn export(materializer: &JadeDocx) -> Result<Vec<u8>, String> {
        materializer.export().map_err(|e| e.to_string())
    }
}

struct Xlsx;

impl Hosted for Xlsx {
    type Doc = XlsxModel;
    type Op = jade_xlsx_collab::XlsxOp;
    type Mat = JadeXlsx;
    const FORMAT: Format = Format::Xlsx;

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

/// Starts a document thread; returns once the document has opened (or failed to).
pub(crate) fn spawn(
    name: &str,
    format: Format,
    bytes: Vec<u8>,
) -> Result<mpsc::Sender<Command>, String> {
    let (commands, inbox) = mpsc::channel();
    let (ready, opened) = mpsc::channel();
    thread::Builder::new()
        .name(format!("doc:{name}"))
        .spawn(move || match format {
            Format::Docx => run::<Docx>(&bytes, inbox, ready),
            Format::Xlsx => run::<Xlsx>(&bytes, inbox, ready),
        })
        .map_err(|e| e.to_string())?;
    opened.recv().map_err(|e| e.to_string())??;
    Ok(commands)
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

fn run<H: Hosted>(
    bytes: &[u8],
    inbox: mpsc::Receiver<Command>,
    ready: mpsc::Sender<Result<(), String>>,
) {
    let (document, materializer) = match H::open(bytes) {
        Ok(opened) => {
            let _ = ready.send(Ok(()));
            opened
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let mut sequencer = Sequencer::with_materializer(document, materializer);
    let mut subscribers: BTreeMap<ReplicaId, Outbox> = BTreeMap::new();

    while let Ok(command) = inbox.recv() {
        match command {
            Command::Join { outbox, reply } => {
                let welcome = sequencer.join();
                let message = ServerMessage::Welcome {
                    format: H::FORMAT,
                    replica: welcome.replica,
                    seq: welcome.seq,
                    snapshot: serde_json::to_value(&welcome.snapshot).expect("models serialize"),
                };
                // Sent from this thread before any later commit, so the
                // replica starts exactly at `seq`.
                let _ = outbox.send(message.to_json());
                subscribers.insert(welcome.replica, outbox);
                let _ = reply.send(welcome.replica);
            }
            Command::Resume {
                replica,
                seq,
                outbox,
                reply,
            } => match sequencer.resume(replica, seq) {
                Ok(resume) => {
                    let message = ServerMessage::Resumed {
                        processed_client_seq: resume.processed_client_seq,
                        missed: resume.missed.into_iter().map(committed_json).collect(),
                    };
                    let _ = outbox.send(message.to_json());
                    subscribers.insert(replica, outbox);
                    let _ = reply.send(Ok(()));
                }
                Err(error) => {
                    let _ = reply.send(Err(error.to_string()));
                }
            },
            Command::Submit {
                replica,
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
                let submission = Submission {
                    client_seq: submission.client_seq,
                    base_seq: submission.base_seq,
                    op,
                };
                match sequencer.submit(replica, submission) {
                    SubmitOutcome::Committed(committed) => {
                        let json = ServerMessage::Committed(committed_json(committed)).to_json();
                        subscribers.retain(|_, outbox| outbox.send(json.clone()).is_ok());
                    }
                    SubmitOutcome::Rejected(rejected) => {
                        if let Some(outbox) = subscribers.get(&replica) {
                            let _ = outbox.send(ServerMessage::Rejected(rejected).to_json());
                        }
                    }
                    SubmitOutcome::Duplicate => {}
                }
            }
            Command::Leave { replica, outbox } => {
                if subscribers
                    .get(&replica)
                    .is_some_and(|current| current.same_channel(&outbox))
                {
                    subscribers.remove(&replica);
                }
            }
            Command::Export { reply } => {
                let _ = reply.send(H::export(sequencer.materializer()));
            }
            Command::Seq { reply } => {
                let _ = reply.send(sequencer.seq());
            }
            Command::Faults { reply } => {
                let _ = reply.send(sequencer.faults().to_vec());
            }
        }
    }
}
