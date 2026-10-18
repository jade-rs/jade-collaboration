//! Differential test against the Lean model of the grid.
//!
//! `lean/JadeCollab/Grid.lean` proves that `apply` keeps a grid valid; its
//! executable `grid-oracle` runs that very `apply`. Feeding the oracle and
//! [`Grid`] the same random operations and comparing every accept/reject
//! decision and the final state ties the proof to this implementation.
//!
//! Needs the oracle binary:
//! `cd lean && lake build grid-oracle` then
//! `JADE_LEAN_ORACLE=lean/.lake/build/bin/grid-oracle cargo test -p jade-collab-core --test lean_oracle -- --ignored`

use std::io::Write;
use std::process::{Command, Stdio};

use jade_collab_core::grid::{Grid, GridOp, Merge};
use jade_collab_core::id::{Id, ReplicaId};
use proptest::prelude::*;

fn id(n: u64) -> Id {
    Id::new(n, ReplicaId(0))
}

fn line(op: &GridOp) -> String {
    let after = |a: &Option<Id>| a.map_or("-".to_string(), |a| a.lamport.to_string());
    match op {
        GridOp::InsertRow { id, after: a } => format!("ir {} {}", id.lamport, after(a)),
        GridOp::DeleteRow { id } => format!("dr {}", id.lamport),
        GridOp::InsertColumn { id, after: a } => format!("ic {} {}", id.lamport, after(a)),
        GridOp::DeleteColumn { id } => format!("dc {}", id.lamport),
        GridOp::Merge(m) => format!(
            "m {} {} {} {}",
            m.top.lamport, m.left.lamport, m.bottom.lamport, m.right.lamport
        ),
        GridOp::Unmerge { top, left } => format!("u {} {}", top.lamport, left.lamport),
    }
}

/// The oracle's output format for a Rust grid.
fn describe(grid: &Grid) -> Vec<String> {
    let axis = |name: &str, all: Vec<Id>, alive: Vec<Id>| {
        let mut out = name.to_string();
        for id in all {
            out.push_str(&format!(
                " {}:{}",
                id.lamport,
                u8::from(alive.contains(&id))
            ));
        }
        out
    };
    let mut merges = "merges".to_string();
    for m in grid.merges() {
        merges.push_str(&format!(
            " {},{},{},{}",
            m.top.lamport, m.left.lamport, m.bottom.lamport, m.right.lamport
        ));
    }
    // Which merge covers each alive cell, row by row.
    let mut cover = "cover".to_string();
    for row in grid.rows() {
        for column in grid.columns() {
            if let Some(m) = grid.merge_at(row, column) {
                cover.push_str(&format!(
                    " {},{}={},{}",
                    row.lamport, column.lamport, m.top.lamport, m.left.lamport
                ));
            }
        }
    }
    vec![
        axis("rows", grid.all_rows(), grid.rows()),
        axis("cols", grid.all_columns(), grid.columns()),
        merges,
        cover,
        if grid.check_invariants().is_ok() {
            "valid"
        } else {
            "invalid"
        }
        .to_string(),
    ]
}

fn oracle(input: &str) -> Vec<String> {
    let path =
        std::env::var("JADE_LEAN_ORACLE").expect("set JADE_LEAN_ORACLE to the grid-oracle binary");
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

/// An operation as selectors, resolved against the grid when it runs: ids
/// are mostly picked among the lines that exist (alive or deleted), so
/// merges, unmerges and deletions hit real cells; sometimes a fresh or
/// unknown id is used instead.
#[derive(Clone, Debug)]
enum Choice {
    InsertRow(u8, u8),
    DeleteRow(u8),
    InsertColumn(u8, u8),
    DeleteColumn(u8),
    Merge(u8, u8, u8, u8),
    Unmerge(u8),
}

fn choice() -> impl Strategy<Value = Choice> {
    let n = any::<u8>();
    prop_oneof![
        2 => (n, n).prop_map(|(a, b)| Choice::InsertRow(a, b)),
        1 => n.prop_map(Choice::DeleteRow),
        2 => (n, n).prop_map(|(a, b)| Choice::InsertColumn(a, b)),
        1 => n.prop_map(Choice::DeleteColumn),
        4 => (n, n, n, n).prop_map(|(a, b, c, d)| Choice::Merge(a, b, c, d)),
        2 => n.prop_map(Choice::Unmerge),
    ]
}

/// A line of `lines` picked by `k`, or now and then an id that is not one.
fn pick(lines: &[Id], k: u8, unknown: u64) -> Id {
    if k.is_multiple_of(13) || lines.is_empty() {
        id(unknown)
    } else {
        lines[k as usize % lines.len()]
    }
}

fn resolve(grid: &Grid, choice: &Choice, fresh: &mut u64) -> GridOp {
    let (rows, columns) = (grid.all_rows(), grid.all_columns());
    let mut new = || {
        *fresh += 1;
        id(*fresh)
    };
    match *choice {
        Choice::InsertRow(a, b) => GridOp::InsertRow {
            id: if a.is_multiple_of(17) {
                pick(&rows, a, 99)
            } else {
                new()
            },
            after: (!b.is_multiple_of(5)).then(|| pick(&rows, b, 98)),
        },
        Choice::DeleteRow(a) => GridOp::DeleteRow {
            id: pick(&rows, a, 97),
        },
        Choice::InsertColumn(a, b) => GridOp::InsertColumn {
            id: if a.is_multiple_of(17) {
                pick(&columns, a, 199)
            } else {
                new()
            },
            after: (!b.is_multiple_of(5)).then(|| pick(&columns, b, 198)),
        },
        Choice::DeleteColumn(a) => GridOp::DeleteColumn {
            id: pick(&columns, a, 197),
        },
        Choice::Merge(a, b, c, d) => GridOp::Merge(Merge {
            top: pick(&rows, a, 96),
            left: pick(&columns, b, 196),
            bottom: pick(&rows, c, 95),
            right: pick(&columns, d, 195),
        }),
        Choice::Unmerge(a) => match grid.merges().get(a as usize % grid.merges().len().max(1)) {
            Some(m) if !a.is_multiple_of(7) => GridOp::Unmerge {
                top: m.top,
                left: m.left,
            },
            _ => GridOp::Unmerge {
                top: pick(&rows, a, 94),
                left: pick(&columns, a.wrapping_add(1), 194),
            },
        },
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    #[ignore = "needs the Lean oracle; see the module docs"]
    fn rust_grid_matches_the_lean_model(
        rows in 1u64..5,
        cols in 1u64..5,
        choices in prop::collection::vec(choice(), 0..50),
    ) {
        let mut grid = Grid::new((1..=rows).map(id), (101..=100 + cols).map(id));
        let mut input = format!(
            "rows {}\ncols {}\n",
            (1..=rows).map(|n| n.to_string()).collect::<Vec<_>>().join(" "),
            (101..=100 + cols).map(|n| n.to_string()).collect::<Vec<_>>().join(" "),
        );
        let mut expected = Vec::new();
        let mut fresh = 1_000;
        for choice in &choices {
            let op = resolve(&grid, choice, &mut fresh);
            input.push_str(&line(&op));
            input.push('\n');
            expected.push(if grid.apply(&op).is_ok() { "ok" } else { "err" }.to_string());
        }
        expected.extend(describe(&grid));
        prop_assert_eq!(oracle(&input), expected, "input:\n{}", input);
    }
}
