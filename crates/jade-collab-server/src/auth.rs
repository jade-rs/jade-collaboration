//! Bearer tokens. A server with [`Tokens`] admits a request only with a
//! configured token, as an `Authorization: Bearer <token>` header or a
//! `?token=<token>` query parameter (browsers cannot set headers on a
//! WebSocket). Viewers read and follow along; editors also write.

use std::path::Path;

use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::Server;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Reads, follows along, and submits operations.
    Editor,
    /// Reads and follows along; every submission is rejected.
    Viewer,
}

/// Who a token belongs to and what it may do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub name: String,
    pub role: Role,
}

impl Grant {
    pub fn may_edit(&self) -> bool {
        self.role == Role::Editor
    }
}

/// The tokens a server accepts: a JSON object mapping each token to a
/// [`Grant`], e.g. `{"s3cret": {"name": "alice", "role": "editor"}}`.
#[derive(Clone, Debug, Default)]
pub struct Tokens {
    grants: Vec<(String, Grant)>,
}

#[derive(Debug, thiserror::Error)]
pub enum TokensError {
    #[error("cannot read {path}: {message}")]
    Read { path: String, message: String },
    #[error("not a token list: {0}")]
    Invalid(String),
}

impl Tokens {
    pub fn from_json(text: &str) -> Result<Self, TokensError> {
        let grants: std::collections::BTreeMap<String, Grant> =
            serde_json::from_str(text).map_err(|e| TokensError::Invalid(e.to_string()))?;
        if grants.contains_key("") {
            return Err(TokensError::Invalid("a token cannot be empty".into()));
        }
        Ok(Self {
            grants: grants.into_iter().collect(),
        })
    }

    pub fn load(path: &Path) -> Result<Self, TokensError> {
        let text = std::fs::read_to_string(path).map_err(|e| TokensError::Read {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        Self::from_json(&text)
    }

    /// The grant of `token`. Every configured token is compared in full, so
    /// the time taken does not tell how much of a guess was right.
    pub fn grant(&self, token: &str) -> Option<&Grant> {
        let mut found = None;
        for (candidate, grant) in &self.grants {
            if constant_time_eq(candidate.as_bytes(), token.as_bytes()) {
                found = Some(grant);
            }
        }
        found
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut difference = a.len() ^ b.len();
    for (i, x) in a.iter().enumerate() {
        difference |= usize::from(x ^ b.get(i).copied().unwrap_or(!x));
    }
    difference == 0
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim().to_string())
}

/// Middleware: lets a request through with its [`Grant`] attached, or
/// answers 401.
pub(crate) async fn authenticate(
    State(server): State<Server>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(tokens) = server.tokens() else {
        return next.run(request).await;
    };
    let presented = bearer(request.headers()).or_else(|| {
        Query::<TokenQuery>::try_from_uri(request.uri())
            .ok()
            .and_then(|query| query.0.token)
    });
    match presented.as_deref().and_then(|token| tokens.grant(token)) {
        Some(grant) => {
            request.extensions_mut().insert(grant.clone());
            next.run(request).await
        }
        None => (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "a valid token is required",
        )
            .into_response(),
    }
}
