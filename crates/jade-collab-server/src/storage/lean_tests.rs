//! Differential stateful test of the file store against the proved Lean model.
use super::*;
use jade_collab_core::id::ReplicaId;
use proptest::prelude::*;
use std::collections::BTreeMap;
use std::process::{Command, Stdio};

fn checkpoint(document: Vec<u64>) -> Checkpoint<Vec<u64>> {
    Checkpoint {
        seq: document.len() as u64,
        processed: BTreeMap::new(),
        owners: BTreeMap::new(),
        next_replica: 1,
        faults: Vec::new(),
        document,
    }
}

fn recovered(stored: Stored<Vec<u64>, u64>) -> Vec<u64> {
    let mut values = stored.checkpoint.document;
    for record in stored.journal {
        match record {
            Record::Committed(committed) => {
                assert_eq!(committed.seq, values.len() as u64 + 1);
                values.push(committed.op);
            }
            _ => panic!("only committed operations in this test"),
        }
    }
    values
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    #[ignore = "requires JADE_LEAN_JOURNAL_ORACLE; see lean/README.md"]
    fn journal_matches_lean_after_batches_tears_checkpoints_and_reopens(
        steps in prop::collection::vec((0u8..4, prop::collection::vec(0..1000u64, 1..5), any::<usize>()), 1..50),
    ) {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("oracle.docx");
        let options = StorageOptions::default();
        let mut store = DocumentStore::create(
            dir.clone(), Format::Docx, options, &checkpoint(Vec::new()), b"file",
        ).unwrap();
        let mut values = Vec::new();
        let mut input = String::new();
        let mut expected = Vec::new();
        for (kind, batch, cut) in steps {
            let numbers = batch.iter().map(ToString::to_string).collect::<Vec<_>>().join(" ");
            match kind {
                0 | 1 => {
                    let records: Vec<_> = batch.iter().enumerate().map(|(i, value)| {
                        let seq = (values.len() + i + 1) as u64;
                        Record::Committed(Committed {
                            seq, origin: ReplicaId::ORIGIN, client_seq: 0, base_seq: seq - 1, op: *value,
                        })
                    }).collect();
                    if kind == 0 {
                        store.append_batch(&records).unwrap();
                        input.push_str(&format!("a {numbers}\n"));
                    } else {
                        let bytes = serde_json::to_vec(&records).unwrap();
                        let cut = cut % (bytes.len() + 1); // never includes a newline
                        OpenOptions::new().append(true).open(dir.join(log_name(store.generation)))
                            .unwrap().write_all(&bytes[..cut]).unwrap();
                        input.push_str(&format!("t {numbers}\n"));
                    }
                }
                2 => {
                    store.checkpoint::<_, u64>(&checkpoint(values.clone()), &[], b"file").unwrap();
                    input.push_str("c\n");
                }
                _ => input.push_str("r\n"),
            }
            let (reopened, stored) = DocumentStore::open::<Vec<u64>, u64>(dir.clone(), Format::Docx, options).unwrap();
            values = recovered(stored);
            expected.push(values.iter().map(ToString::to_string).collect::<Vec<_>>().join(" "));
            store = reopened;
        }
        let path = std::env::var("JADE_LEAN_JOURNAL_ORACLE").expect("set JADE_LEAN_JOURNAL_ORACLE");
        let mut child = Command::new(path).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let output = child.wait_with_output().unwrap();
        prop_assert!(output.status.success());
        let actual = String::from_utf8(output.stdout).unwrap();
        prop_assert_eq!(actual.lines().collect::<Vec<_>>(), expected);
    }
}
