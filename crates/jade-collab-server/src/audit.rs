//! The audit log: one JSON object per line for every access — who
//! connected to which document with which role and as which replica, when
//! and why it left, what was refused, which files went in and out. A token
//! is recorded by its name, never by itself, and request URLs are not
//! recorded (a socket may carry its token in the query).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

#[derive(Clone, Default)]
pub(crate) struct Audit(Option<Arc<Mutex<File>>>);

impl Audit {
    /// Appends to the file at `path`, creating it.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self(Some(Arc::new(Mutex::new(file)))))
    }

    /// Records `event` with `fields` (an object), stamped with the time in
    /// milliseconds since the Unix epoch. A write that fails is reported on
    /// stderr; it never stops the server.
    pub fn record(&self, event: &str, fields: Value) {
        let Some(file) = &self.0 else {
            return;
        };
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let mut entry = Map::new();
        entry.insert("at".into(), at.into());
        entry.insert("event".into(), event.into());
        if let Value::Object(fields) = fields {
            entry.extend(fields);
        }
        let mut line = Value::Object(entry).to_string();
        line.push('\n');
        let mut file = file.lock().unwrap();
        if let Err(error) = file.write_all(line.as_bytes()).and_then(|()| file.flush()) {
            eprintln!("cannot write the audit log: {error}");
        }
    }
}
