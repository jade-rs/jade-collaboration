//! Stateful differential test against the bounded allocation model Lean proves.
use jade_collab_core::id::{Clock, Id, ReplicaId};
use proptest::prelude::*;
use std::io::Write;
use std::process::{Command, Stdio};

fn boundary() -> impl Strategy<Value = u64> {
    prop_oneof![
        0..32u64,
        any::<u64>(),
        (0..32u64).prop_map(|n| u64::MAX - n)
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]
    #[test]
    #[ignore = "requires JADE_LEAN_ID_ORACLE; see lean/README.md"]
    fn checked_identifiers_match_the_lean_model(
        steps in prop::collection::vec((0u8..3, boundary(), boundary()), 1..150),
    ) {
        let mut clock = Clock::new(ReplicaId(1));
        let mut input = String::new();
        let mut expected = Vec::new();
        for (kind, value, count) in steps {
            match kind {
                0 => {
                    clock.observe_lamport(value);
                    input.push_str(&format!("o {value}\n"));
                    expected.push(format!("o {}", clock.now()));
                }
                1 => {
                    input.push_str(&format!("r {count}\n"));
                    expected.push(match clock.try_reserve(count) {
                        Ok(first) => format!("ok {} {}", first.lamport, clock.now()),
                        Err(_) => format!("err {}", clock.now()),
                    });
                }
                _ => {
                    input.push_str(&format!("i {value} {count}\n"));
                    expected.push(match Id::new(value, ReplicaId(2)).last_in_run(count) {
                        Ok(last) => format!("id {}", last.lamport),
                        Err(_) => "err".into(),
                    });
                }
            }
        }
        let path = std::env::var("JADE_LEAN_ID_ORACLE").expect("set JADE_LEAN_ID_ORACLE");
        let mut child = Command::new(path).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let output = child.wait_with_output().unwrap();
        prop_assert!(output.status.success());
        let actual = String::from_utf8(output.stdout).unwrap();
        prop_assert_eq!(actual.lines().collect::<Vec<_>>(), expected);
    }
}
