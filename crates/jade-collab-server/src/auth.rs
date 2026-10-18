//! Bearer tokens. A server with [`Tokens`] admits a request only with a
//! configured token, as an `Authorization: Bearer <token>` header or a
//! `?token=<token>` query parameter (browsers cannot set headers on a
//! WebSocket). Viewers read and follow along; editors also write. A token
//! may be limited to some documents, with a role of its own on each.

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
    /// The role on every document, unless `documents` says otherwise.
    pub role: Role,
    /// The documents the token opens; every document when absent. The first
    /// rule whose pattern matches a name decides the role there; a name no
    /// rule matches is closed to the token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documents: Option<Vec<DocumentRule>>,
}

/// Documents a token opens: names matching a pattern — the whole name, `*`
/// standing for any run of characters and `?` for exactly one — with the
/// token's role or one of its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DocumentRule {
    /// `"deck-*.pptx"`: with the token's role.
    Pattern(String),
    /// `{"document": "contract.docx", "role": "viewer"}`.
    WithRole(DocumentRole),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentRole {
    pub document: String,
    pub role: Role,
}

impl DocumentRule {
    fn pattern(&self) -> &str {
        match self {
            DocumentRule::Pattern(pattern) => pattern,
            DocumentRule::WithRole(rule) => &rule.document,
        }
    }
}

impl Grant {
    /// The token's role on `document`, or `None` if it may not open it.
    pub fn role_for(&self, document: &str) -> Option<Role> {
        let Some(rules) = &self.documents else {
            return Some(self.role);
        };
        rules.iter().find_map(|rule| {
            glob_matches(rule.pattern(), document).then_some(match rule {
                DocumentRule::Pattern(_) => self.role,
                DocumentRule::WithRole(rule) => rule.role,
            })
        })
    }
}

/// Whether `name` matches `pattern` as a whole; `*` matches any run of
/// characters, `?` exactly one, everything else itself.
fn glob_matches(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut p, mut n) = (0, 0);
    // The last `*` seen, and where in the name it has been tried up to:
    // on a mismatch it takes one more character.
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, n));
                p += 1;
            }
            Some(&c) if c == '?' || c == name[n] => {
                p += 1;
                n += 1;
            }
            _ => match star {
                Some((star_p, star_n)) => {
                    star = Some((star_p, star_n + 1));
                    p = star_p + 1;
                    n = star_n + 1;
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

/// The tokens a server accepts: a JSON object mapping each token to a
/// [`Grant`], e.g. `{"s3cret": {"name": "alice", "role": "editor"}}` or
/// `{"t0ken": {"name": "bot", "role": "viewer", "documents": ["*.xlsx",
/// {"document": "draft.docx", "role": "editor"}]}}`.
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
        for grant in grants.values() {
            let rules = grant.documents.iter().flatten();
            if rules.into_iter().any(|rule| rule.pattern().is_empty()) {
                return Err(TokensError::Invalid(format!(
                    "{}: a document pattern cannot be empty",
                    grant.name
                )));
            }
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

/// What `grant` (none on a server without tokens) may do with the document
/// `name`; `None` when nothing.
pub(crate) fn access(grant: Option<&Grant>, name: &str) -> Option<Role> {
    match grant {
        None => Some(Role::Editor),
        Some(grant) => grant.role_for(name),
    }
}

/// The token a request presented, attached to it next to its [`Grant`].
#[derive(Clone)]
pub(crate) struct Presented(pub String);

/// What `token` may do with `document` under `tokens`, or why nothing.
pub(crate) fn recheck(
    tokens: &Tokens,
    token: &str,
    document: &str,
) -> Result<(Grant, Role), String> {
    let grant = tokens
        .grant(token)
        .ok_or_else(|| "the token was withdrawn".to_string())?;
    let role = grant
        .role_for(document)
        .ok_or_else(|| format!("the token no longer opens {document}"))?;
    Ok((grant.clone(), role))
}

/// The answer to a known token without access to `name`.
pub(crate) fn forbidden(name: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        format!("this token has no access to {name}"),
    )
        .into_response()
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

/// Middleware: lets a request through with its [`Grant`] (and the token,
/// for sockets to keep checking) attached, or answers 401.
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
    let granted = presented.and_then(|token| Some((tokens.grant(&token)?.clone(), token)));
    match granted {
        Some((grant, token)) => {
            request.extensions_mut().insert(grant);
            request.extensions_mut().insert(Presented(token));
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

#[cfg(test)]
mod tests {
    use super::glob_matches;

    #[test]
    fn globs_match_whole_names() {
        for (pattern, name, matches) in [
            ("*", "", true),
            ("*", "a.docx", true),
            ("*.docx", "a.docx", true),
            ("*.docx", "a.docx.xlsx", false),
            ("a*b*c", "aXbYbZc", true),
            ("a*b*c", "aXbYbZ", false),
            ("?", "", false),
            ("??", "ab", true),
            ("deck-?.pptx", "deck-1.pptx", true),
            ("deck-?.pptx", "deck-10.pptx", false),
            ("contract.docx", "contract.docx", true),
            ("contract.docx", "Contract.docx", false),
            ("**a", "ba", true),
            ("*a*", "bbb", false),
            ("ü*", "über.docx", true),
        ] {
            assert_eq!(glob_matches(pattern, name), matches, "{pattern} ~ {name}");
        }
    }
}
