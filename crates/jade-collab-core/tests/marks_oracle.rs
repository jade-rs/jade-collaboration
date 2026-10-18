//! Differential test against the Lean model of marks.
//!
//! `lean/JadeCollab/Marks.lean` proves that formatting is a function of the
//! sets of inserts and marks and that it follows the expand rules; its
//! executable `marks-oracle` runs that very `effective` over the Lean RGA's
//! walk. Feeding the oracle and [`Rga`] + [`MarkSet`] the same random
//! histories (marks made with [`anchors`], so with both expand rules) and
//! comparing the formatting of every atom ties the proofs to this code.
//!
//! Needs the oracle binary:
//! `cd lean && lake build marks-oracle` then
//! `JADE_LEAN_MARKS_ORACLE=lean/.lake/build/bin/marks-oracle cargo test -p jade-collab-core --test marks_oracle -- --ignored`

use std::io::Write;
use std::process::{Command, Stdio};

use jade_collab_core::id::{Clock, Id, ReplicaId};
use jade_collab_core::marks::{anchors, Boundary, Expand, Mark, MarkSet};
use jade_collab_core::rga::Rga;
use proptest::prelude::*;

const KEYS: u8 = 3;

fn boundary(b: Boundary) -> String {
    match b {
        Boundary::Start => "s".to_string(),
        Boundary::End => "e".to_string(),
        Boundary::Before(a) => format!("b:{}:{}", a.lamport, a.replica.0),
        Boundary::After(a) => format!("a:{}:{}", a.lamport, a.replica.0),
    }
}

/// `None` (back to the base formatting) is 0 for the oracle, `Some(v)` is
/// `v + 1`.
fn value(v: Option<u8>) -> u64 {
    v.map_or(0, |v| v as u64 + 1)
}

fn oracle(input: &str) -> Vec<String> {
    let path = std::env::var("JADE_LEAN_MARKS_ORACLE")
        .expect("set JADE_LEAN_MARKS_ORACLE to the marks-oracle binary");
    let mut child = Command::new(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("run the oracle");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "oracle failed on:\n{input}");
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|l| l.trim_end().to_string())
        .collect()
}

#[derive(Clone, Debug)]
enum Choice {
    Insert {
        peer: usize,
        at: usize,
    },
    Mark {
        peer: usize,
        from: usize,
        len: usize,
        key: u8,
        value: Option<u8>,
        expand: bool,
    },
    Sync {
        from: usize,
        to: usize,
    },
}

fn choice() -> impl Strategy<Value = Choice> {
    prop_oneof![
        5 => (0..3usize, any::<usize>()).prop_map(|(peer, at)| Choice::Insert { peer, at }),
        3 => (0..3usize, any::<usize>(), 0..5usize, 0..KEYS, prop::option::of(0..3u8), any::<bool>())
            .prop_map(|(peer, from, len, key, value, expand)| Choice::Mark { peer, from, len, key, value, expand }),
        2 => (0..3usize, 0..3usize).prop_map(|(from, to)| Choice::Sync { from, to }),
    ]
}

#[derive(Clone)]
enum Op {
    Insert(Id, Option<Id>),
    Mark(Mark<u8, u8>),
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    #[ignore = "needs the Lean oracle; see the module docs"]
    fn rust_marks_match_the_lean_model(choices in prop::collection::vec(choice(), 0..60)) {
        // Three replicas, each with its own view; ops are shared by syncs.
        let mut log: Vec<Op> = Vec::new();
        let mut views: Vec<(Rga<char>, Clock, usize)> = (0..3)
            .map(|p| (Rga::new(), Clock::new(ReplicaId(p + 1)), 0))
            .collect();
        // A view holds a prefix of the log (`.2` ops), so it is causal.
        let catch_up = |view: &mut (Rga<char>, Clock, usize), log: &[Op], upto: usize| {
            while view.2 < upto {
                if let Op::Insert(id, parent) = &log[view.2] {
                    view.1.observe(*id);
                    view.0.insert(*id, *parent, 'x').unwrap();
                } else if let Op::Mark(m) = &log[view.2] {
                    view.1.observe(m.id);
                }
                view.2 += 1;
            }
        };
        for choice in &choices {
            match *choice {
                Choice::Insert { peer, at } => {
                    let upto = log.len();
                    catch_up(&mut views[peer], &log, upto);
                    let view = &mut views[peer];
                    let len = view.0.len();
                    let parent = (at % (len + 1) != len).then(|| view.0.nodes()[at % (len + 1)].id);
                    let id = view.1.tick();
                    log.push(Op::Insert(id, parent));
                }
                Choice::Mark { peer, from, len, key, value, expand } => {
                    let view = &mut views[peer];
                    let n = view.0.len();
                    if n == 0 {
                        continue;
                    }
                    let from = from % n;
                    let to = (from + len).min(n - 1);
                    let nodes = view.0.nodes();
                    let (first, last) = (nodes[from].id, nodes[to].id);
                    let mode = if expand { Expand::End } else { Expand::None };
                    let (start, end) = anchors(&view.0, first, last, mode).unwrap();
                    let id = view.1.tick();
                    log.push(Op::Mark(Mark::new(id, key, start, end, value)));
                }
                Choice::Sync { from, to } => {
                    let upto = views[from].2;
                    if upto > views[to].2 {
                        catch_up(&mut views[to], &log, upto);
                    }
                }
            }
        }
        let mut rga: Rga<char> = Rga::new();
        let mut marks: MarkSet<u8, u8> = MarkSet::new();
        let mut input = String::new();
        for op in &log {
            match op {
                Op::Insert(id, parent) => {
                    rga.insert(*id, *parent, 'x').unwrap();
                    let parent = parent.map_or("-".to_string(), |p| format!("{} {}", p.lamport, p.replica.0));
                    input.push_str(&format!("i {} {} {}\n", id.lamport, id.replica.0, parent));
                }
                Op::Mark(m) => {
                    marks.insert(m.clone()).unwrap();
                    input.push_str(&format!(
                        "m {} {} {} {} {} {}\n",
                        m.id.lamport, m.id.replica.0, m.key, value(m.value),
                        boundary(m.start), boundary(m.end)
                    ));
                }
            }
        }
        input.push_str(&format!("k {}\n", (0..KEYS).map(|k| k.to_string()).collect::<Vec<_>>().join(" ")));
        // The sweep's spans, atom by atom.
        let mut table = std::collections::BTreeMap::new();
        for span in marks.effective(&rga) {
            for index in span.range.clone() {
                table.insert((index, span.key), (span.id, span.value));
            }
        }
        let expected: Vec<String> = rga
            .iter()
            .enumerate()
            .map(|(index, node)| {
                let mut line = format!("{}:{}", node.id.lamport, node.id.replica.0);
                for key in 0..KEYS {
                    match table.get(&(index, key)) {
                        None => line.push_str(" -"),
                        Some((id, v)) => line.push_str(&format!(
                            " {}:{}:{}", id.lamport, id.replica.0, value(*v)
                        )),
                    }
                }
                line
            })
            .collect();
        prop_assert_eq!(oracle(&input), expected, "input:\n{}", input);
    }
}
