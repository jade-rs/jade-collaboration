# jade-collab-proofs

Lean 4 proofs of the properties `docs/design.md` §7 lists for the
*specifications* of jade-collaboration. Core Lean only (no Mathlib), no
`sorry`, no extra axioms (`#print axioms` shows at most `propext`,
`Classical.choice`, `Quot.sound`).

## Build

```sh
cd lean
lake build
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
