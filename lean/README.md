# jade-collab-proofs

Lean 4 proofs of the properties `docs/design.md` §7 lists for the
*specifications* of jade-collaboration. Core Lean only (no Mathlib), no
`sorry`, no extra axioms (`#print axioms` shows at most `propext`,
`Classical.choice`, `Quot.sound`).

## Build

```sh
cd lean
lake build            # the proofs and the grid-oracle and marks-oracle executables
```

The toolchain is pinned in `lean-toolchain` (`leanprover/lean4:v4.34.0`).

## What is proved

### `JadeCollab/Rga.lean`: RGA specification (`rga::spec`)

The state is a set of inserts `Node { id, parent, value }` and a set of deleted
ids. The document is a depth-first walk of the parent tree, with siblings in
descending `(lamport, replica, value)` order and tombstones filtered out.

- `children_perm`, `walk_perm`: sibling order and the walk do not depend on
  the order in which inserts arrived.
- `state_nodup`, `state_mem`, `state_perm`: delivering inserts is idempotent
  set insertion. Replicas that received the same inserts hold permutations of
  the same list.
- `render_congr`: `render` depends only on the insert set and the deletion
  set.
- `converges`: replicas that delivered the same inserts and the same
  deletions, in any order and with any duplication, render the same document
  (strong eventual consistency).

### `JadeCollab/Lww.lean`: last-writer-wins registers (`LwwRegister`)

- `merge_comm`, `merge_assoc`, `merge_idem`: `merge` is a join semilattice.
- `write_comm`, `write_idem`: concurrent writes commute, and re-delivering a
  write changes nothing.
- `converges`: the same writes, in any order and with any duplication, give
  the same register.

### `JadeCollab/Marks.lean`: marks over the RGA (`jade_collab_core::marks`)

A mark gives attribute `key` a value between two boundaries — a side of an
atom (`before a`, `after a`) or an end of the story (`start`, `stop`). An
atom is covered when it lies strictly between them in the story's order
(tombstones included, so anchors never vanish); per attribute it shows the
covering mark with the greatest identifier, as a `Lww` write.

- `effective_congr`, `converges`, `deliver_comm`: an atom's formatting is a
  function of the *set* of mark operations (any order, any duplication).
- `format_converges`, `richRender_converges`: with `Rga.converges`, replicas
  that received the same inserts, deletions and marks show the same text with
  the same formatting; deletions do not enter the formatting.
- `covers_preserved`, `effective_preserved`: atoms inserted later (any
  number, anywhere, as long as old atoms keep their relative order, which the
  RGA guarantees) never change the formatting of the atoms already there.
- `covers_in_gap`, `effective_in_gap`, `covers_at_start`: an atom that lands
  in the gap after `l` gets exactly the formatting of the marks spanning that
  gap — whoever inserted it and whenever it arrived.
- `markOver_covers_iff`, `typed_inside`, `typed_after_end`,
  `typed_before_start`, `typed_at_story_start`: the expand rules of the marks
  the intent API makes (`before first` … `before` the next atom or `stop`
  when the attribute expands, `after last` when it does not): an atom typed
  inside the range is covered, one typed right after the last atom is
  covered exactly when the attribute expands, one typed right before the
  first atom never is.

### `JadeCollab/Sequencer.lean`: why tables are sequenced

- `sequenced_preserves`: a sequencer that rejects the operations it cannot
  apply, leaving the state unchanged, keeps any invariant that every
  successful operation keeps.
- `replicas_agree`: catching up on a log in chunks gives the same state as the
  server replaying the whole log. `same_log_same_state` is the trivial form.
- `crdt_union_breaks_invariant`: merge A (rows 0–1, cols 0–1) and merge B
  (rows 1–2, cols 1–2) are each valid on an empty table. Their CRDT union
  overlaps, so it is not valid (design.md §1, first row).
- `merge_sequenced_valid`: merges that go through the sequencer always leave a
  valid set of merge regions.

### `JadeCollab/Reconcile.lean`: the client fast path

- `fast_path_sound`: suppose a remote operation commutes with every
  operation. Applying it straight to the optimistic view
  `pending.foldl step confirmed` then gives the same result as rebuilding the
  view on the new confirmed state. `fast_path_sound_many` extends this to a
  batch of such operations.
- `quiescent_view_eq_server`, `quiescent_converges`: with nothing pending and
  the server's log fully folded into the confirmed state, the client's view
  equals the server's state.

### `JadeCollab/Grid.lean`: the table grid (`jade_collab_core::grid::Grid`)

A line-for-line model of `Grid` with `Nat` identifiers. `apply : Grid → Op →
Option Grid` takes the same accept/reject decisions as `Grid::apply` and leaves
the same state: an inserted row goes to `position(after) + 1` (or `0`), a new
merge is pushed at the end, `Unmerge` removes the first merge with that
top-left, and a deletion tombstones the line and then normalizes every merge
(corners moved to the first/last alive line between them, merges with fewer
than two alive cells dropped). `Valid` is exactly what `check_invariants`
checks: identifiers unique per axis, an alive row and column, every merge
passing `check_merge`, and no merge overlapping an earlier one (on raw
positions).

- `apply_preserves_valid : Valid g → apply g op = some g' → Valid g'`: every
  accepted operation keeps the invariant. Insertions shift positions
  monotonically (`pos_insertAt`, `sh_le_sh`), so `overlaps` is unchanged and
  alive-cell counts only grow (`cnt_insertAt`). Normalization after a deletion
  only shrinks a merge inside its old rectangle onto alive corners
  (`normalizeOne_spec`), and sub-rectangles of disjoint rectangles stay
  disjoint (`overlaps_normalizeOne`). Columns are handled as the rows of the
  transposed grid (`apply_insertCol`, `apply_deleteCol`, `Valid.tr`).
- `apply_rejects_unchanged`: a rejected operation leaves the sequencer's state
  as it was (`apply_accepts` is the other case).
- `grid_sequenced_valid`, `grid_replicas_valid`: any log of grid operations
  folded by the sequencer (`Sequencer.step apply`), in any number of chunks,
  leaves a valid grid.
- `check_iff_valid`: the executable `check` (the loops of `check_invariants`)
  decides `Valid`.
- Rows and columns through merged regions, as in Word and Excel, stated on
  the cells a merge covers (`between`, `covers`, `Holds`):
  `insertRow_covers` / `insertCol_covers` — merges keep their corners, every
  other line is inside or outside each merge as before, and the new line is
  inside a merge exactly when inserted after one of its lines other than the
  last (strictly inside: the merge grows; at its edge: it does not);
  `deleteRow_covers` / `deleteCol_covers` — a surviving merge covers exactly
  its old alive cells minus the deleted line (an edge moves inward;
  `aliveSpan_bounds`, `normalizeOne_holds`), and a merge is dropped exactly
  when fewer than two cells are left between its corners
  (`normalizeOne_none`, via `cnt_aliveSpan`).
- `mergeAt_spec`: `mergeAt` (`Grid::merge_at`) returns the merge covering an
  alive cell; in a valid grid there is at most one.
- `sanity_*`: the unit tests of `crates/jade-collab-core/tests/grid.rs`,
  evaluated by `decide` (insert after an anchor, deleted rows still anchor
  inserts, last row/column kept, overlapping and malformed merges rejected,
  deleting an anchor row shrinks a merge and a one-cell merge disappears,
  unmerge, the naive union of two merges is invalid, rows and columns
  inserted inside or at the edge of a merge, edges moving inward).

### `JadeCollab/CellCommute.lean`: cell contents are convergent

A table is a grid plus contents for every `(row, column)` pair; a cell edit
(any function of the old contents) is accepted iff the row and the column exist,
alive or deleted.

- `apply_ids`: grid operations never remove identifiers; the only ones they
  add are the row or column an insertion creates.
- `cell_edit_comm_grid`: a cell edit valid where it was made commutes with
  every grid operation — applying it before or after gives the same result,
  both rejected or both the same table.
- `cell_edit_after_grid_iff`: the edit is accepted after a grid operation iff
  before, unless that operation created the edited row or column
  (`cell_edit_needs_existing_row` shows the side condition is needed).
  `grid_after_cell_edit`: a grid operation's outcome does not depend on cell
  edits.
- `step_comm_edit_grid`: the sequencer steps of a valid cell edit and any grid
  operation commute. `cell_edit_in_deleted_row`: an edit to a cell of a
  deleted row still lands.

### `JadeCollab/Anchor.lean`: tables stay anchored to visible marks

A story of characters and paragraph marks (with tombstones) and tables
anchored after a mark or at the story start. Convergent: insert a character,
insert a mark (split), delete a character (never a mark). Sequenced: join
(delete a mark, refused while a live table hangs off it), insert a table after
a visible mark, delete a table.

- `apply_preserves_anchored`, `anchored_sequenced`: every live table's anchor
  mark exists and is visible, after any accepted operation and any sequenced
  log.
- `convergent_preserves_sequenced_validity`: a convergent operation never
  changes whether a join, a table insertion or a table deletion is accepted,
  as long as the sequenced operation does not name the mark the convergent one
  creates (`join_needs_existing_mark` shows why that side condition is
  needed). `join_blocked_by_table` is a concrete run of the guard.

## `grid-oracle`

`lake build` also builds `.lake/build/bin/grid-oracle` (`GridOracle.lean`),
which runs the same `apply`, `check` and `mergeAt` as `Grid.lean` on a line protocol, for
differential testing against the Rust `Grid`:

```
rows <id> <id> ...                 initial rows, all alive, in order
cols <id> <id> ...                 initial columns
ir <id> <after|->                  InsertRow (`-`: at the top)
dr <id>                            DeleteRow
ic <id> <after|->                  InsertColumn (`-`: at the left)
dc <id>                            DeleteColumn
m <top> <left> <bottom> <right>    Merge
u <top> <left>                     Unmerge
```

It prints `ok` or `err` per operation, then `rows <id>:<1|0> ...` and
`cols <id>:<1|0> ...` (every line ever inserted, in order, with its alive
flag), `merges <top>,<left>,<bottom>,<right> ...` (in list order; the bare word
when there are none), `cover <row>,<col>=<top>,<left> ...` (`mergeAt` of every
alive cell a merge covers, row by row) and `valid` or `invalid`. A malformed line is reported on
stderr with exit code 2.

```sh
$ printf 'rows 1 2 3\ncols 101\nm 1 101 3 101\ndr 1\ndr 3\n' | lake exe grid-oracle
ok
ok
ok
rows 1:0 2:1 3:0
cols 101:1
merges
cover
valid
```

## `marks-oracle`

`lake build` also builds `.lake/build/bin/marks-oracle` (`MarksOracle.lean`):
the RGA walk and `Marks.effective` on a line protocol, for differential
testing against the Rust `Rga` and `MarkSet`
(`crates/jade-collab-core/tests/marks_oracle.rs`):

```
i <lamport> <replica> <parent-lamport> <parent-replica>   insert after an atom
i <lamport> <replica> -                                   insert at the start
m <lamport> <replica> <key> <value> <start> <end>         mark; boundaries s | b:<l>:<r> | a:<l>:<r> | e
k <key> <key> ...                                         attributes to report
```

It prints one line per atom in story order: `<lamport>:<replica>`, then per
attribute `-` or the winning mark as `<lamport>:<replica>:<value>`.

```sh
$ printf 'i 1 1 -\ni 2 1 1 1\ni 3 1 2 1\ni 4 2 2 1\nm 10 1 0 1 b:1:1 b:3:1\nk 0\n' | lake exe marks-oracle
1:1 10:1:1
2:1 10:1:1
4:2 10:1:1
3:1 -
```
