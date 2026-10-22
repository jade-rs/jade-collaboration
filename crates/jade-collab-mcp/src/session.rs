//! One joined document: a replica connected to the server, and the tools'
//! work on it.

use std::time::Duration;

use jade_collab_core::id::{Id, IdError, ReplicaId};
use jade_collab_server::{Client, ClientError, ClientSnapshot, Format, Inspection};
use jade_collab_sync::{Document, ReplicaEvent};
use jade_docx_collab::intent::{try_resolve_with as resolve_docx, word_date, Context, IntentOp};
use jade_docx_collab::{DocxFormatting, DocxModel};
use jade_pptx_collab::intent::{try_resolve as resolve_pptx, PptxIntent};
use jade_pptx_collab::{PptxFormatting, PptxModel};
use jade_xlsx_collab::intent::{try_resolve as resolve_xlsx, XlsxIntent};
use jade_xlsx_collab::XlsxModel;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use crate::read::{read_docx, read_inspected_docx, read_inspected_pptx, read_pptx, read_xlsx};
use crate::receipts::{Outcome, Receipt, Verdict};

pub(crate) enum Session {
    Docx(Client<DocxModel>),
    Xlsx(Client<XlsxModel>),
    Pptx(Client<PptxModel>),
}

pub(crate) enum ReadSnapshot {
    Docx(Box<ClientSnapshot<DocxModel>>),
    InspectedDocx(Box<ClientSnapshot<DocxModel>>, DocxFormatting),
    Xlsx(ClientSnapshot<XlsxModel>),
    Pptx(ClientSnapshot<PptxModel>),
    InspectedPptx(ClientSnapshot<PptxModel>, PptxFormatting),
}

impl ReadSnapshot {
    pub(crate) fn includes_base_formatting(&self) -> bool {
        matches!(self, Self::InspectedDocx(..) | Self::InspectedPptx(..))
    }
    pub(crate) fn read(&self, start: usize, limit: usize) -> Value {
        let (mut value, seq, pending) = match self {
            Self::Docx(s) => (read_docx(&s.view, start, limit), s.seq, s.pending),
            Self::InspectedDocx(s, formatting) => (
                read_inspected_docx(&s.view, formatting, start, limit),
                s.seq,
                0,
            ),
            Self::Xlsx(s) => (read_xlsx(&s.view, start, limit), s.seq, s.pending),
            Self::Pptx(s) => (read_pptx(&s.view, start, limit), s.seq, s.pending),
            Self::InspectedPptx(s, formatting) => (
                read_inspected_pptx(&s.view, formatting, start, limit),
                s.seq,
                0,
            ),
        };
        value["seq"] = json!(seq);
        value["pending"] = json!(pending);
        value
    }
}

/// Resolves one edit against a view into operations on behalf of a replica;
/// `ids(n)` reserves `n` fresh identifiers.
type Resolve<D> = fn(
    &D,
    Value,
    ReplicaId,
    &mut dyn FnMut(u64) -> Result<Id, IdError>,
) -> Result<Vec<<D as Document>::Op>, String>;

/// docx edits are the agent's own: with tracking on, its deletions remove
/// its own tracked insertions outright, and its changes are dated now.
fn docx_ops(
    view: &DocxModel,
    edit: Value,
    replica: ReplicaId,
    ids: &mut dyn FnMut(u64) -> Result<Id, IdError>,
) -> Result<Vec<<DocxModel as Document>::Op>, String> {
    let intent: IntentOp = serde_json::from_value(edit).map_err(|e| e.to_string())?;
    let context = Context {
        replica: Some(replica),
        date: Some(word_date(std::time::SystemTime::now())),
    };
    resolve_docx(view, &intent, &context, ids).map_err(|e| e.to_string())
}

fn xlsx_ops(
    view: &XlsxModel,
    edit: Value,
    _: ReplicaId,
    ids: &mut dyn FnMut(u64) -> Result<Id, IdError>,
) -> Result<Vec<<XlsxModel as Document>::Op>, String> {
    let intent: XlsxIntent = serde_json::from_value(edit).map_err(|e| e.to_string())?;
    resolve_xlsx(view, &intent, ids)
        .map(|op| vec![op])
        .map_err(|e| e.to_string())
}

fn pptx_ops(
    view: &PptxModel,
    edit: Value,
    _: ReplicaId,
    ids: &mut dyn FnMut(u64) -> Result<Id, IdError>,
) -> Result<Vec<<PptxModel as Document>::Op>, String> {
    let intent: PptxIntent = serde_json::from_value(edit).map_err(|e| e.to_string())?;
    resolve_pptx(view, &intent, ids).map_err(|e| e.to_string())
}

fn connection_lost(error: ClientError) -> String {
    format!("the connection to the document failed: {error}")
}

async fn inspect<D>(client: &Client<D>) -> Result<Inspection<D>, String>
where
    D: Document + DeserializeOwned + Send + 'static,
    D::Op: Serialize + DeserializeOwned + Send + 'static,
{
    tokio::time::timeout(
        Duration::from_millis(crate::DEFAULT_TIMEOUT_MS),
        client.inspect(),
    )
    .await
    .map_err(|_| "native formatting inspection timed out; retry the read".to_owned())?
    .map_err(connection_lost)
}

/// Plans and queues under the replica lock. The caller retains the receipt
/// before awaiting network verdicts, including for canceled waits.
fn submit<D>(
    client: &Client<D>,
    edits: Vec<Value>,
    resolve: Resolve<D>,
    expected: Option<&ClientSnapshot<D>>,
) -> Result<Receipt, String>
where
    D: Document + DeserializeOwned + Send + 'static,
    D::Op: Serialize + DeserializeOwned + Send + 'static,
{
    if let Some(error) = client.connection_error() {
        return Err(connection_lost(error));
    }
    let outcomes = client
        .edit(expected, |context| {
            edits
                .into_iter()
                .map(|edit| {
                    let view = context.view().clone();
                    let ops =
                        match resolve(&view, edit, context.id(), &mut |n| context.try_reserve(n)) {
                            Ok(ops) => ops,
                            Err(error) => return Outcome::Invalid(error),
                        };
                    let mut tickets = Vec::with_capacity(ops.len());
                    let mut refusal = None;
                    for op in ops {
                        match context.submit(op) {
                            Ok(ticket) => tickets.push(ticket),
                            Err(error) => {
                                refusal = Some(error.to_string());
                                break;
                            }
                        }
                    }
                    Outcome::Submitted { tickets, refusal }
                })
                .collect()
        })
        .map_err(|e| e.to_string())?;
    Ok(Receipt::new(outcomes))
}

fn answers<Op>(events: Vec<ReplicaEvent<Op>>) -> Vec<(u64, Verdict)> {
    events
        .into_iter()
        .filter_map(|event| match event {
            ReplicaEvent::Acknowledged { client_seq } => Some((client_seq, Verdict::Applied)),
            ReplicaEvent::Rejected {
                client_seq, reason, ..
            } => Some((client_seq, Verdict::Rejected(reason.to_string()))),
            ReplicaEvent::Applied { .. } => None,
        })
        .collect()
}

/// Waits until `client` has received the log beyond `after`, or `timeout`.
async fn wait<D>(client: &Client<D>, after: u64, timeout: Duration) -> Result<Value, String>
where
    D: Document + DeserializeOwned + Send + 'static,
    D::Op: Serialize + DeserializeOwned + Send + 'static,
{
    if let Ok(waited) =
        tokio::time::timeout(timeout, client.wait_for_seq(after.saturating_add(1))).await
    {
        waited.map_err(connection_lost)?;
    }
    let seq = client.seq();
    Ok(json!({ "changed": seq > after, "seq": seq }))
}

impl Session {
    pub(crate) async fn join(
        url: &str,
        format: Format,
        token: Option<&str>,
    ) -> Result<Self, String> {
        let connect = |e: ClientError| format!("cannot join the document: {e}");
        Ok(match format {
            Format::Docx => Session::Docx(Client::connect(url, token).await.map_err(connect)?),
            Format::Xlsx => Session::Xlsx(Client::connect(url, token).await.map_err(connect)?),
            Format::Pptx => Session::Pptx(Client::connect(url, token).await.map_err(connect)?),
        })
    }

    pub(crate) fn replica(&self) -> u64 {
        match self {
            Session::Docx(client) => client.id().0,
            Session::Xlsx(client) => client.id().0,
            Session::Pptx(client) => client.id().0,
        }
    }

    pub(crate) fn seq(&self) -> u64 {
        match self {
            Session::Docx(client) => client.seq(),
            Session::Xlsx(client) => client.seq(),
            Session::Pptx(client) => client.seq(),
        }
    }

    pub(crate) fn pending(&self) -> usize {
        match self {
            Session::Docx(client) => client.pending_len(),
            Session::Xlsx(client) => client.pending_len(),
            Session::Pptx(client) => client.pending_len(),
        }
    }

    pub(crate) fn snapshot(&self) -> ReadSnapshot {
        match self {
            Session::Docx(client) => ReadSnapshot::Docx(Box::new(client.snapshot())),
            Session::Xlsx(client) => ReadSnapshot::Xlsx(client.snapshot()),
            Session::Pptx(client) => ReadSnapshot::Pptx(client.snapshot()),
        }
    }

    pub(crate) async fn inspected_snapshot(&self) -> Result<ReadSnapshot, String> {
        match self {
            Session::Docx(client) => {
                let inspected = inspect(client).await?;
                let formatting: DocxFormatting = serde_json::from_value(inspected.formatting)
                    .map_err(|e| format!("invalid native formatting: {e}"))?;
                formatting.validate(&inspected.snapshot.view)?;
                Ok(ReadSnapshot::InspectedDocx(Box::new(inspected.snapshot), formatting))
            }
            Session::Pptx(client) => {
                let inspected = inspect(client).await?;
                let formatting: PptxFormatting = serde_json::from_value(inspected.formatting)
                    .map_err(|e| format!("invalid native formatting: {e}"))?;
                formatting.validate(&inspected.snapshot.view)?;
                Ok(ReadSnapshot::InspectedPptx(inspected.snapshot, formatting))
            }
            Session::Xlsx(_) => Err("include_base_formatting is available for DOCX and PPTX only; XLSX base styles are included in ordinary reads".into()),
        }
    }

    pub(crate) fn submit(
        &self,
        edits: Vec<Value>,
        expected: Option<&ReadSnapshot>,
    ) -> Result<Receipt, String> {
        match self {
            Session::Docx(client) => {
                let expected = match expected {
                    None => None,
                    Some(ReadSnapshot::Docx(s)) => Some(s.as_ref()),
                    Some(ReadSnapshot::InspectedDocx(s, _)) => Some(s.as_ref()),
                    _ => return Err("snapshot format mismatch".into()),
                };
                submit(client, edits, docx_ops, expected)
            }
            Session::Xlsx(client) => {
                let expected = match expected {
                    None => None,
                    Some(ReadSnapshot::Xlsx(s)) => Some(s),
                    _ => return Err("snapshot format mismatch".into()),
                };
                submit(client, edits, xlsx_ops, expected)
            }
            Session::Pptx(client) => {
                let expected = match expected {
                    None => None,
                    Some(ReadSnapshot::Pptx(s)) => Some(s),
                    Some(ReadSnapshot::InspectedPptx(s, _)) => Some(s),
                    _ => return Err("snapshot format mismatch".into()),
                };
                submit(client, edits, pptx_ops, expected)
            }
        }
    }

    pub(crate) fn answers(&self) -> Vec<(u64, Verdict)> {
        match self {
            Self::Docx(client) => answers(client.take_events()),
            Self::Xlsx(client) => answers(client.take_events()),
            Self::Pptx(client) => answers(client.take_events()),
        }
    }

    pub(crate) async fn settle_through(&self, ticket: u64) -> Result<(), String> {
        match self {
            Self::Docx(client) => client.settle_through(ticket).await,
            Self::Xlsx(client) => client.settle_through(ticket).await,
            Self::Pptx(client) => client.settle_through(ticket).await,
        }
        .map_err(connection_lost)
    }

    pub(crate) async fn wait(&self, after: u64, timeout: Duration) -> Result<Value, String> {
        match self {
            Session::Docx(client) => wait(client, after, timeout).await,
            Session::Xlsx(client) => wait(client, after, timeout).await,
            Session::Pptx(client) => wait(client, after, timeout).await,
        }
    }

    /// Closes the connection; returns how many edits were unanswered.
    pub(crate) async fn leave(self) -> usize {
        let unanswered = self.pending();
        match self {
            Session::Docx(client) => drop(client.disconnect().await),
            Session::Xlsx(client) => drop(client.disconnect().await),
            Session::Pptx(client) => drop(client.disconnect().await),
        }
        unanswered
    }
}
