//! The audit log: one JSON object per line for every access — who
//! connected to which document with which role and as which replica, when
//! and why it left, what was refused, which files went in and out. A token
//! is recorded by its name, never by itself, and request URLs are not
//! recorded (a socket may carry its token in the query).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

#[derive(Clone, Default)]
pub(crate) struct Audit(Option<Arc<Log>>);

struct Log {
    path: PathBuf,
    file: Mutex<File>,
}

fn append(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

impl Audit {
    /// Appends to the file at `path`, creating it.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        Ok(Self(Some(Arc::new(Log {
            path: path.to_path_buf(),
            file: Mutex::new(append(path)?),
        }))))
    }

    /// Where the log is written, if there is one.
    pub fn path(&self) -> Option<&Path> {
        self.0.as_ref().map(|log| log.path.as_path())
    }

    /// Opens the file at the log's path again, for when it was rotated
    /// (moved away) — what logrotate asks for with SIGHUP. Entries go on
    /// to the old file if the new one cannot be opened.
    pub fn reopen(&self) -> std::io::Result<()> {
        let Some(log) = &self.0 else {
            return Ok(());
        };
        let file = append(&log.path)?;
        *log.file.lock().unwrap() = file;
        Ok(())
    }

    /// Records `event` with `fields` (an object), stamped with the time in
    /// milliseconds since the Unix epoch. A write that fails is reported on
    /// stderr; it never stops the server.
    pub fn record(&self, event: &str, fields: Value) {
        let Some(log) = &self.0 else {
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
        let mut file = log.file.lock().unwrap();
        if let Err(error) = file.write_all(line.as_bytes()).and_then(|()| file.flush()) {
            eprintln!("cannot write the audit log: {error}");
        }
    }
}
