//! One joined document: a replica connected to the server, and the tools'
//! work on it.

use std::collections::HashMap;
use std::time::Duration;

use jade_collab_core::id::Id;
use jade_collab_server::{Client, ClientError, Format};
use jade_collab_sync::{Document, ReplicaEvent};
use jade_docx_collab::intent::{resolve as resolve_docx, IntentOp};
use jade_docx_collab::DocxModel;
use jade_xlsx_collab::intent::{resolve as resolve_xlsx, XlsxIntent};
use jade_xlsx_collab::XlsxModel;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use crate::read::{read_docx, read_xlsx};

pub(crate) enum Session {
    Docx(Client<DocxModel>),
    Xlsx(Client<XlsxModel>),
}

/// What became of one edit before the server answered.
enum Outcome {
    /// It could not be parsed or resolved against the view.
    Invalid(String),
    /// The local replica refused one of its operations.
    Rejected(String),
    /// Its operations' client sequence numbers.
    Submitted(Vec<u64>),
}

/// Resolves one edit against a view into operations; `ids(n)` reserves `n`
/// fresh identifiers.
type Resolve<D> =
    fn(&D, Value, &mut dyn FnMut(u64) -> Id) -> Result<Vec<<D as Document>::Op>, String>;

fn docx_ops(
    view: &DocxModel,
    edit: Value,
    ids: &mut dyn FnMut(u64) -> Id,
) -> Result<Vec<<DocxModel as Document>::Op>, String> {
    let intent: IntentOp = serde_json::from_value(edit).map_err(|e| e.to_string())?;
    resolve_docx(view, &intent, ids).map_err(|e| e.to_string())
}

fn xlsx_ops(
    view: &XlsxModel,
    edit: Value,
    ids: &mut dyn FnMut(u64) -> Id,
) -> Result<Vec<<XlsxModel as Document>::Op>, String> {
    let intent: XlsxIntent = serde_json::from_value(edit).map_err(|e| e.to_string())?;
    resolve_xlsx(view, &intent, ids)
        .map(|op| vec![op])
        .map_err(|e| e.to_string())
}

fn connection_lost(error: ClientError) -> String {
    format!("the connection to the document failed: {error}")
}

/// Submits `edits` in order, each resolved against the view the previous
/// ones left, then waits up to `timeout` for the server to answer them.
async fn apply<D>(
    client: &Client<D>,
    edits: Vec<Value>,
    timeout: Duration,
    resolve: Resolve<D>,
) -> Result<Value, String>
where
    D: Document + DeserializeOwned + Send + 'static,
    D::Op: Serialize + DeserializeOwned + Send + 'static,
{
    let mut outcomes = Vec::with_capacity(edits.len());
    for edit in edits {
        let view = client.view();
        let ops = match resolve(&view, edit, &mut |n| client.reserve(n)) {
            Ok(ops) => ops,
            Err(error) => {
                outcomes.push(Outcome::Invalid(error));
                continue;
            }
        };
        let mut submitted = Vec::with_capacity(ops.len());
        let mut refused = None;
        for op in ops {
            match client.submit(op) {
                Ok(client_seq) => submitted.push(client_seq),
                Err(rejection) => {
                    // The rest of the edit depends on this operation.
                    refused = Some(rejection.to_string());
                    break;
                }
            }
        }
        outcomes.push(match refused {
            Some(reason) => Outcome::Rejected(reason),
            None => Outcome::Submitted(submitted),
        });
    }
    if let Ok(settled) = tokio::time::timeout(timeout, client.settle()).await {
        settled.map_err(connection_lost)?;
    }
    // Answers by client sequence number: `None` committed, `Some` refused.
    let answers: HashMap<u64, Option<String>> = client
        .take_events()
        .into_iter()
        .filter_map(|event| match event {
            ReplicaEvent::Acknowledged { client_seq } => Some((client_seq, None)),
            ReplicaEvent::Rejected {
                client_seq, reason, ..
            } => Some((client_seq, Some(reason.to_string()))),
            ReplicaEvent::Applied { .. } => None,
        })
        .collect();
    let results: Vec<Value> = outcomes
        .into_iter()
        .enumerate()
        .map(|(index, outcome)| match outcome {
            Outcome::Invalid(error) => {
                json!({ "index": index, "status": "invalid", "error": error })
            }
            Outcome::Rejected(reason) => {
                json!({ "index": index, "status": "rejected", "reason": reason })
            }
            Outcome::Submitted(client_seqs) => {
                let refused = client_seqs
                    .iter()
                    .find_map(|seq| answers.get(seq).cloned().flatten());
                match refused {
                    Some(reason) => {
                        json!({ "index": index, "status": "rejected", "reason": reason })
                    }
                    None if client_seqs.iter().all(|seq| answers.contains_key(seq)) => {
                        json!({ "index": index, "status": "applied" })
                    }
                    None => json!({ "index": index, "status": "pending" }),
                }
            }
        })
        .collect();
    Ok(json!({
        "results": results,
        "seq": client.seq(),
        "pending": client.pending_len(),
    }))
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
    pub(crate) async fn join(url: &str, format: Format) -> Result<Self, String> {
        let connect = |e: ClientError| format!("cannot join {url}: {e}");
        Ok(match format {
            Format::Docx => Session::Docx(Client::connect(url).await.map_err(connect)?),
            Format::Xlsx => Session::Xlsx(Client::connect(url).await.map_err(connect)?),
        })
    }

    pub(crate) fn replica(&self) -> u64 {
        match self {
            Session::Docx(client) => client.id().0,
            Session::Xlsx(client) => client.id().0,
        }
    }

    pub(crate) fn seq(&self) -> u64 {
        match self {
            Session::Docx(client) => client.seq(),
            Session::Xlsx(client) => client.seq(),
        }
    }

    pub(crate) fn pending(&self) -> usize {
        match self {
            Session::Docx(client) => client.pending_len(),
            Session::Xlsx(client) => client.pending_len(),
        }
    }

    pub(crate) fn read(&self, start: usize, limit: usize) -> Value {
        // seq and pending are read before the view so they never claim more
        // than the view shows.
        let (seq, pending) = (self.seq(), self.pending());
        let mut read = match self {
            Session::Docx(client) => read_docx(&client.view(), start, limit),
            Session::Xlsx(client) => read_xlsx(&client.view(), start, limit),
        };
        read["seq"] = json!(seq);
        read["pending"] = json!(pending);
        read
    }

    pub(crate) async fn apply(
        &self,
        edits: Vec<Value>,
        timeout: Duration,
    ) -> Result<Value, String> {
        match self {
            Session::Docx(client) => apply(client, edits, timeout, docx_ops).await,
            Session::Xlsx(client) => apply(client, edits, timeout, xlsx_ops).await,
        }
    }

    pub(crate) async fn wait(&self, after: u64, timeout: Duration) -> Result<Value, String> {
        match self {
            Session::Docx(client) => wait(client, after, timeout).await,
            Session::Xlsx(client) => wait(client, after, timeout).await,
        }
    }

    /// Closes the connection; returns how many edits were unanswered.
    pub(crate) async fn leave(self) -> usize {
        let unanswered = self.pending();
        match self {
            Session::Docx(client) => drop(client.disconnect().await),
            Session::Xlsx(client) => drop(client.disconnect().await),
        }
        unanswered
    }
}
