//! The JSON messages on a document's WebSocket. Operations and snapshots are
//! carried as JSON values so one connection type serves every format.

use jade_collab_core::id::ReplicaId;
use jade_collab_sync::protocol::{Committed, Rejected, Submission};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Docx,
    Xlsx,
    Pptx,
}

impl Format {
    /// The format a document name's extension declares.
    pub fn of(name: &str) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".docx") {
            Some(Format::Docx)
        } else if lower.ends_with(".xlsx") {
            Some(Format::Xlsx)
        } else if lower.ends_with(".pptx") {
            Some(Format::Pptx)
        } else {
            None
        }
    }

    /// The format of the document a socket URL
    /// (`ws://host/documents/{name}/ws`) names.
    pub fn of_url(url: &str) -> Option<Self> {
        let path = url.trim_end_matches('/');
        let path = path.strip_suffix("/ws").unwrap_or(path);
        Self::of(path.rsplit('/').next()?)
    }

    /// The file extension, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Docx => "docx",
            Format::Xlsx => "xlsx",
            Format::Pptx => "pptx",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Become a new replica; answered by `welcome`.
    Join,
    /// Come back as `replica`, last at log position `seq`; answered by
    /// `resumed`. Resend unanswered submissions afterwards.
    Resume {
        replica: ReplicaId,
        seq: u64,
    },
    Submit(Submission<Value>),
    /// Ephemeral state for the others to see (a cursor, a selection, …);
    /// replaces this replica's previous presence. Never logged.
    Presence {
        state: Value,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Welcome {
        format: Format,
        replica: ReplicaId,
        seq: u64,
        snapshot: Value,
    },
    Resumed {
        processed_client_seq: u64,
        missed: Vec<Committed<Value>>,
    },
    Committed(Committed<Value>),
    Rejected(Rejected),
    /// Another replica's presence; `name` is its token's name, if the server
    /// has tokens.
    Presence {
        replica: ReplicaId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        state: Value,
    },
    /// Another replica's connection closed (or it reconnected): its presence
    /// is gone.
    PresenceLeft {
        replica: ReplicaId,
    },
    /// The server cannot resume the replica (it never joined, or its log
    /// position was compacted into a checkpoint); join again.
    Rejoin {
        message: String,
    },
    Error {
        message: String,
    },
}

impl ServerMessage {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("server messages serialize")
    }
}
