//! Durable documents. Each document has a directory under the server's
//! storage directory, named after the document:
//!
//! ```text
//! <dir>/<name>/snapshot-<n>.json   {"format": "docx", "checkpoint": <Checkpoint>, "tail": [<Committed>…]}
//! <dir>/<name>/snapshot-<n>.docx   the file jade exported at the checkpoint
//! <dir>/<name>/log-<n>.jsonl       one journal `Record` per line, after it
//! ```
//!
//! `<n>` counts checkpoints. The checkpoint holds the model itself, not just
//! the file: model ids are minted when a file is loaded, so a model derived
//! again from the exported file would not match the ids every replica holds.
//! The tail is the last committed operations before the checkpoint, kept so
//! that replicas slightly behind it can still resume.
//! A record is appended (handed to the operating system and, with
//! [`Fsync::EveryRecord`], synced to the disk) before anyone hears of it, so
//! no replica can be ahead of the journal. A new checkpoint is complete once
//! its `.json` is renamed into place; older generations are then removed.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use jade_collab_sync::protocol::Committed;
use jade_collab_sync::{Checkpoint, Record};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::wire::Format;

/// When the journal is synced to the disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fsync {
    /// After every record, before anyone hears of it: an operation that was
    /// acknowledged survives a power failure. One `fsync` per record.
    EveryRecord,
    /// Only checkpoints are synced. Records are handed to the operating
    /// system at once, so they survive the server process dying, but a
    /// power failure can lose the records since the last checkpoint.
    #[default]
    Checkpoints,
}

/// How a durable server keeps its documents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorageOptions {
    /// Take a checkpoint (a new snapshot, and an empty journal) after this
    /// many journal records — joins, commits and rejections.
    pub checkpoint_every: u64,
    /// Keep this many committed operations from before each checkpoint, in
    /// memory and in the snapshot, so that a replica at most this far
    /// behind can always resume instead of joining again.
    pub keep_tail: usize,
    pub sync: Fsync,
}

impl Default for StorageOptions {
    fn default() -> Self {
        Self {
            checkpoint_every: 1000,
            keep_tail: 1000,
            sync: Fsync::default(),
        }
    }
}

/// Where journal records go: a file, or in tests something that watches
/// what is done to one.
pub(crate) trait JournalFile {
    /// Hands `bytes` to the operating system.
    fn append(&mut self, bytes: &[u8]) -> io::Result<()>;
    /// Waits until everything appended is on the disk.
    fn sync(&mut self) -> io::Result<()>;
}

impl JournalFile for File {
    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.write_all(bytes).and_then(|()| self.flush())
    }

    fn sync(&mut self) -> io::Result<()> {
        self.sync_data()
    }
}

/// Turns each journal file a store opens into what it appends to.
pub(crate) type JournalOpener = Arc<dyn Fn(File) -> Box<dyn JournalFile>>;

fn plain_journal() -> JournalOpener {
    Arc::new(|file| Box::new(file))
}

#[derive(Serialize)]
struct SnapshotOut<'a, D, Op> {
    format: Format,
    checkpoint: &'a Checkpoint<D>,
    tail: &'a [Committed<Op>],
}

#[derive(Deserialize)]
struct SnapshotIn<D, Op> {
    format: Format,
    checkpoint: Checkpoint<D>,
    /// Absent in snapshots written before tails were kept.
    #[serde(default = "Vec::new")]
    tail: Vec<Committed<Op>>,
}

/// What a document directory holds, read back.
pub(crate) struct Stored<D, Op> {
    pub checkpoint: Checkpoint<D>,
    /// The committed operations right before the checkpoint.
    pub tail: Vec<Committed<Op>>,
    pub file: Vec<u8>,
    pub journal: Vec<Record<Op>>,
}

/// One document's directory, open for appending.
pub(crate) struct DocumentStore {
    dir: PathBuf,
    format: Format,
    options: StorageOptions,
    generation: u64,
    log: Box<dyn JournalFile>,
    journal: JournalOpener,
    records: u64,
}

fn error(path: &Path, error: impl std::fmt::Display) -> String {
    format!("{}: {error}", path.display())
}

fn snapshot_json(generation: u64) -> String {
    format!("snapshot-{generation}.json")
}

fn snapshot_file(generation: u64, format: Format) -> String {
    format!("snapshot-{generation}.{}", format.extension())
}

fn log_name(generation: u64) -> String {
    format!("log-{generation}.jsonl")
}

/// The number in `snapshot-<n>.json`, `snapshot-<n>.<ext>` or `log-<n>.jsonl`.
fn generation_of(file_name: &str) -> Option<u64> {
    let rest = file_name
        .strip_prefix("snapshot-")
        .or_else(|| file_name.strip_prefix("log-"))?;
    rest.split('.').next()?.parse().ok()
}

/// The newest complete checkpoint in `dir`.
pub(crate) fn newest_generation(dir: &Path) -> Result<Option<u64>, String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(error(dir, e)),
    };
    let mut newest = None;
    for entry in entries {
        let name = entry.map_err(|e| error(dir, e))?.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with("snapshot-") && name.ends_with(".json") {
            newest = newest.max(generation_of(name));
        }
    }
    Ok(newest)
}

/// Writes `bytes` to `path` so that a crash leaves either the old file or
/// the new one.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    let mut file = File::create(&temporary).map_err(|e| error(&temporary, e))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| error(&temporary, e))?;
    fs::rename(&temporary, path).map_err(|e| error(path, e))
}

fn sync_dir(dir: &Path) -> Result<(), String> {
    // Makes the renames durable; not every platform can open a directory.
    match File::open(dir) {
        Ok(handle) => handle.sync_all().or(Ok(())),
        Err(_) => Ok(()),
    }
}

/// Writes checkpoint number `generation` and returns its (empty) journal,
/// open for appending. The `.json` goes last: until it is in place the
/// previous generation stays the newest complete one.
fn write_generation<D: Serialize, Op: Serialize>(
    dir: &Path,
    format: Format,
    generation: u64,
    checkpoint: &Checkpoint<D>,
    tail: &[Committed<Op>],
    file: &[u8],
) -> Result<File, String> {
    write_atomically(&dir.join(snapshot_file(generation, format)), file)?;
    let log_path = dir.join(log_name(generation));
    let log = File::create(&log_path)
        .and_then(|log| log.sync_all().map(|()| log))
        .map_err(|e| error(&log_path, e))?;
    let json = serde_json::to_vec(&SnapshotOut {
        format,
        checkpoint,
        tail,
    })
    .expect("checkpoints serialize");
    write_atomically(&dir.join(snapshot_json(generation)), &json)?;
    sync_dir(dir)?;
    Ok(log)
}

impl DocumentStore {
    /// Starts a document's directory with its first checkpoint.
    pub fn create<D: Serialize>(
        dir: PathBuf,
        format: Format,
        options: StorageOptions,
        checkpoint: &Checkpoint<D>,
        file: &[u8],
    ) -> Result<Self, String> {
        Self::create_with(dir, format, options, checkpoint, file, plain_journal())
    }

    /// [`DocumentStore::create`], appending through `journal`.
    pub fn create_with<D: Serialize>(
        dir: PathBuf,
        format: Format,
        options: StorageOptions,
        checkpoint: &Checkpoint<D>,
        file: &[u8],
        journal: JournalOpener,
    ) -> Result<Self, String> {
        if newest_generation(&dir)?.is_some() {
            return Err(format!("{} already holds a document", dir.display()));
        }
        fs::create_dir_all(&dir).map_err(|e| error(&dir, e))?;
        let log = write_generation::<D, ()>(&dir, format, 1, checkpoint, &[], file)?;
        Ok(Self {
            dir,
            format,
            options,
            generation: 1,
            log: journal(log),
            journal,
            records: 0,
        })
    }

    /// Reads a document's directory back and opens its journal for appending.
    /// A torn last line — the process died while appending it — is dropped.
    pub fn open<D: DeserializeOwned, Op: DeserializeOwned>(
        dir: PathBuf,
        format: Format,
        options: StorageOptions,
    ) -> Result<(Self, Stored<D, Op>), String> {
        Self::open_with(dir, format, options, plain_journal())
    }

    /// [`DocumentStore::open`], appending through `journal`.
    pub fn open_with<D: DeserializeOwned, Op: DeserializeOwned>(
        dir: PathBuf,
        format: Format,
        options: StorageOptions,
        opener: JournalOpener,
    ) -> Result<(Self, Stored<D, Op>), String> {
        let generation = newest_generation(&dir)?
            .ok_or_else(|| format!("{} holds no snapshot", dir.display()))?;
        let json_path = dir.join(snapshot_json(generation));
        let json = fs::read(&json_path).map_err(|e| error(&json_path, e))?;
        let snapshot: SnapshotIn<D, Op> =
            serde_json::from_slice(&json).map_err(|e| error(&json_path, e))?;
        if snapshot.format != format {
            return Err(error(
                &json_path,
                format!("holds a {:?} document", snapshot.format),
            ));
        }
        let file_path = dir.join(snapshot_file(generation, format));
        let file = fs::read(&file_path).map_err(|e| error(&file_path, e))?;

        let log_path = dir.join(log_name(generation));
        let text = match fs::read(&log_path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(error(&log_path, e)),
        };
        let complete = text.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        let mut records = Vec::new();
        for (number, line) in text[..complete].split(|b| *b == b'\n').enumerate() {
            if line.is_empty() {
                continue;
            }
            let record = serde_json::from_slice(line)
                .map_err(|e| error(&log_path, format!("line {}: {e}", number + 1)))?;
            records.push(record);
        }
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| error(&log_path, e))?;
        if complete < text.len() {
            log.set_len(complete as u64)
                .map_err(|e| error(&log_path, e))?;
        }
        let store = Self {
            dir,
            format,
            options,
            generation,
            log: opener(log),
            journal: opener,
            records: records.len() as u64,
        };
        store.remove_older_generations()?;
        Ok((
            store,
            Stored {
                checkpoint: snapshot.checkpoint,
                tail: snapshot.tail,
                file,
                journal: records,
            },
        ))
    }

    /// Appends a record, and syncs it if the options say so. Returns whether
    /// a checkpoint is due.
    pub fn append<Op: Serialize>(&mut self, record: &Record<Op>) -> Result<bool, String> {
        let mut line = serde_json::to_vec(record).expect("records serialize");
        line.push(b'\n');
        let path = self.dir.join(log_name(self.generation));
        self.log.append(&line).map_err(|e| error(&path, e))?;
        if self.options.sync == Fsync::EveryRecord {
            self.log.sync().map_err(|e| error(&path, e))?;
        }
        self.records += 1;
        Ok(self.records >= self.options.checkpoint_every)
    }

    /// How many committed operations to keep before each checkpoint.
    pub fn keep_tail(&self) -> usize {
        self.options.keep_tail
    }

    /// Replaces the snapshot with `checkpoint`, the `tail` of the log kept
    /// before it, and `file`, and starts an empty journal.
    pub fn checkpoint<D: Serialize, Op: Serialize>(
        &mut self,
        checkpoint: &Checkpoint<D>,
        tail: &[Committed<Op>],
        file: &[u8],
    ) -> Result<(), String> {
        let generation = self.generation + 1;
        let log = write_generation(&self.dir, self.format, generation, checkpoint, tail, file)?;
        self.log = (self.journal)(log);
        self.generation = generation;
        self.records = 0;
        self.remove_older_generations()
    }

    fn remove_older_generations(&self) -> Result<(), String> {
        for entry in fs::read_dir(&self.dir).map_err(|e| error(&self.dir, e))? {
            let path = entry.map_err(|e| error(&self.dir, e))?.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let stale =
                generation_of(name).is_some_and(|g| g < self.generation) || name.ends_with(".tmp");
            if stale {
                fs::remove_file(&path).map_err(|e| error(&path, e))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jade_collab_core::id::ReplicaId;
    use jade_collab_sync::protocol::Committed;
    use proptest::prelude::*;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Debug)]
    enum Step {
        Append(Record<String>),
        /// The process dies while appending: part of a line, no newline.
        Tear(Vec<u8>),
        Reopen,
    }

    fn record() -> impl Strategy<Value = Record<String>> {
        prop_oneof![
            (1..9u64).prop_map(|r| Record::Joined {
                replica: ReplicaId(r)
            }),
            (1..9u64, any::<u64>()).prop_map(|(r, client_seq)| Record::Rejected {
                replica: ReplicaId(r),
                client_seq
            }),
            (any::<u64>(), ".*").prop_map(|(seq, op)| Record::Committed(Committed {
                seq,
                origin: ReplicaId(1),
                client_seq: seq,
                base_seq: 0,
                op,
            })),
        ]
    }

    fn steps() -> impl Strategy<Value = Vec<Step>> {
        prop::collection::vec(
            prop_oneof![
                8 => record().prop_map(Step::Append),
                1 => prop::collection::vec(any::<u8>().prop_filter("no newline", |b| *b != b'\n'), 1..40)
                    .prop_map(Step::Tear),
                2 => Just(Step::Reopen),
            ],
            0..60,
        )
    }

    fn checkpoint(seq: u64) -> Checkpoint<String> {
        Checkpoint {
            seq,
            processed: BTreeMap::from([(ReplicaId(1), seq)]),
            next_replica: 2,
            faults: vec![format!("fault {seq}")],
            document: format!("document at {seq}"),
        }
    }

    fn fsync() -> impl Strategy<Value = Fsync> {
        prop_oneof![Just(Fsync::EveryRecord), Just(Fsync::Checkpoints)]
    }

    #[derive(Clone, Debug, PartialEq)]
    enum Event {
        Append(Vec<u8>),
        Sync,
    }

    /// A journal file that tells what was done to it.
    struct Recorder {
        file: File,
        events: Arc<Mutex<Vec<Event>>>,
    }

    impl JournalFile for Recorder {
        fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.events
                .lock()
                .unwrap()
                .push(Event::Append(bytes.to_vec()));
            self.file.append(bytes)
        }

        fn sync(&mut self) -> io::Result<()> {
            self.events.lock().unwrap().push(Event::Sync);
            self.file.sync()
        }
    }

    fn recording(events: &Arc<Mutex<Vec<Event>>>) -> JournalOpener {
        let events = events.clone();
        Arc::new(move |file| {
            Box::new(Recorder {
                file,
                events: events.clone(),
            })
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        /// Every record reaches the disk before `append` returns — and so
        /// before anyone hears of it — under `EveryRecord`; under
        /// `Checkpoints` the journal is written to the operating system but
        /// never synced, across checkpoints and reopenings.
        #[test]
        fn the_journal_is_synced_as_the_policy_says(
            sync in fsync(),
            checkpoint_every in 1..6u64,
            steps in prop::collection::vec(
                prop_oneof![8 => record().prop_map(Some), 1 => Just(None)],
                0..40,
            ),
        ) {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("doc.docx");
            let options = StorageOptions { checkpoint_every, sync, ..StorageOptions::default() };
            let events = Arc::new(Mutex::new(Vec::new()));
            let mut store = DocumentStore::create_with(
                dir.clone(), Format::Docx, options, &checkpoint(0), b"file", recording(&events),
            ).unwrap();
            let mut appended = 0;
            for step in steps {
                let Some(record) = step else {
                    store = DocumentStore::open_with::<String, String>(
                        dir.clone(), Format::Docx, options, recording(&events),
                    ).unwrap().0;
                    continue;
                };
                let before = events.lock().unwrap().len();
                let due = store.append(&record).unwrap();
                appended += 1;
                let mut line = serde_json::to_vec(&record).unwrap();
                line.push(b'\n');
                let happened = events.lock().unwrap()[before..].to_vec();
                match sync {
                    Fsync::EveryRecord => prop_assert_eq!(happened, vec![Event::Append(line), Event::Sync]),
                    Fsync::Checkpoints => prop_assert_eq!(happened, vec![Event::Append(line)]),
                }
                if due {
                    store.checkpoint::<String, String>(&checkpoint(appended), &[], b"file").unwrap();
                }
            }
            let syncs = events.lock().unwrap().iter().filter(|e| **e == Event::Sync).count();
            prop_assert_eq!(syncs, if sync == Fsync::EveryRecord { appended as usize } else { 0 });
        }

        /// Whatever was appended, checkpointed and torn, reopening gives the
        /// newest checkpoint, the tail kept with it, its file, and exactly
        /// the complete records after it — and keeps one generation on disk.
        #[test]
        fn reopening_reads_back_the_newest_checkpoint_and_journal(
            checkpoint_every in 1..8u64,
            keep_tail in 0..4usize,
            sync in fsync(),
            steps in steps(),
        ) {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("doc.docx");
            let options = StorageOptions { checkpoint_every, keep_tail, sync };
            let mut checkpoints = 0;
            let mut expected_checkpoint = checkpoint(checkpoints);
            let mut store = DocumentStore::create(
                dir.clone(), Format::Docx, options, &expected_checkpoint, b"file 0",
            ).unwrap();
            let mut expected_file = b"file 0".to_vec();
            let mut expected_tail: Vec<Committed<String>> = Vec::new();
            let mut expected_journal: Vec<Record<String>> = Vec::new();
            for step in steps {
                match step {
                    Step::Append(record) => {
                        let due = store.append(&record).unwrap();
                        expected_journal.push(record);
                        prop_assert_eq!(due, expected_journal.len() as u64 >= checkpoint_every);
                        if due {
                            checkpoints += 1;
                            expected_checkpoint = checkpoint(checkpoints);
                            expected_file = format!("file {checkpoints}").into_bytes();
                            let committed: Vec<Committed<String>> = expected_journal
                                .iter()
                                .filter_map(|record| match record {
                                    Record::Committed(c) => Some(c.clone()),
                                    _ => None,
                                })
                                .collect();
                            expected_tail = committed[committed.len().saturating_sub(keep_tail)..].to_vec();
                            store.checkpoint(&expected_checkpoint, &expected_tail, &expected_file).unwrap();
                            expected_journal.clear();
                        }
                    }
                    Step::Tear(bytes) => {
                        store.log.append(&bytes).unwrap();
                        let (reopened, stored) =
                            DocumentStore::open::<String, String>(dir.clone(), Format::Docx, options).unwrap();
                        store = reopened;
                        prop_assert_eq!(&stored.journal, &expected_journal);
                    }
                    Step::Reopen => {
                        let (reopened, stored) =
                            DocumentStore::open::<String, String>(dir.clone(), Format::Docx, options).unwrap();
                        store = reopened;
                        prop_assert_eq!(&stored.checkpoint, &expected_checkpoint);
                        prop_assert_eq!(&stored.tail, &expected_tail);
                        prop_assert_eq!(&stored.file, &expected_file);
                        prop_assert_eq!(&stored.journal, &expected_journal);
                    }
                }
            }
            let (_, stored) =
                DocumentStore::open::<String, String>(dir.clone(), Format::Docx, options).unwrap();
            prop_assert_eq!(stored.checkpoint, expected_checkpoint);
            prop_assert_eq!(stored.tail, expected_tail);
            prop_assert_eq!(stored.file, expected_file);
            prop_assert_eq!(stored.journal, expected_journal);
            let mut names: Vec<String> = fs::read_dir(&dir).unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            let n = checkpoints + 1;
            prop_assert_eq!(names, vec![log_name(n), snapshot_file(n, Format::Docx), snapshot_json(n)]);
        }
    }

    #[test]
    fn a_snapshot_written_before_tails_existed_reads_back_with_none() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("doc.docx");
        fs::create_dir_all(&dir).unwrap();
        let json = serde_json::json!({"format": "docx", "checkpoint": checkpoint(3)});
        fs::write(dir.join(snapshot_json(1)), json.to_string()).unwrap();
        fs::write(dir.join(snapshot_file(1, Format::Docx)), b"file").unwrap();
        let (_, stored) =
            DocumentStore::open::<String, String>(dir, Format::Docx, StorageOptions::default())
                .unwrap();
        assert_eq!(stored.checkpoint, checkpoint(3));
        assert!(stored.tail.is_empty());
    }
}
