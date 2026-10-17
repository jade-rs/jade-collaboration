/-!
# Why tables need the central sequencer

design.md §1–§2: a CRDT guarantees convergence, not validity. Merge regions of
a table are not invariant-confluent (`crdt_union_breaks_invariant`), whereas a
sequencer that validates each operation against the authoritative state and
rejects the invalid ones preserves *any* validity predicate
(`sequenced_preserves`). Backs `Sequencer` in `jade-collab-sync` and
`Grid::apply` / `Grid::check_invariants` in
`crates/jade-collab-core/src/grid.rs`.
-/

namespace JadeCollab.Sequencer

section Generic

variable {S Op : Type} (apply : S → Op → Option S)

/-- One sequencer step: commit the operation if it applies, otherwise reject
it and leave the state unchanged. -/
def step (s : S) (op : Op) : S :=
  (apply s op).getD s

/-- **The sequencer preserves any invariant** that every successful operation
preserves, whatever the order or content of the log (cited by
`crates/jade-collab-core/tests/grid.rs`; a rejected op leaves the grid as it
was). -/
theorem sequenced_preserves (Inv : S → Prop)
    (hop : ∀ s op s', Inv s → apply s op = some s' → Inv s') :
    ∀ (ops : List Op) (s : S), Inv s → Inv (ops.foldl (step apply) s) := by
  intro ops
  induction ops with
  | nil => intro s hs; exact hs
  | cons op ops ih =>
    intro s hs
    apply ih
    unfold step
    cases h : apply s op with
    | none => exact hs
    | some s' => exact hop s op s' hs h

/-- **Replicas agree with the server**: a replica that applied a prefix `l₁` of
the log and then the rest `l₂` holds exactly the server's state for
`l₁ ++ l₂`. The sequencer's step is a deterministic function of the state, so
catching up in any number of chunks is the same as replaying the whole log. -/
theorem replicas_agree (s : S) (l₁ l₂ : List Op) :
    (l₁ ++ l₂).foldl (step apply) s = l₂.foldl (step apply) (l₁.foldl (step apply) s) :=
  List.foldl_append

/-- Two replicas folding the same log from the same snapshot are equal. -/
theorem same_log_same_state (s : S) {l₁ l₂ : List Op} (h : l₁ = l₂) :
    l₁.foldl (step apply) s = l₂.foldl (step apply) s := by
  rw [h]

end Generic

/-! ## Merge regions on an index grid -/

/-- A merge region: rows `top..=bottom`, columns `left..=right`. -/
structure Rect where
  top : Nat
  bottom : Nat
  left : Nat
  right : Nat
deriving DecidableEq, Repr

/-- Well-formed: ordered corners and at least two cells. -/
def Rect.wf (r : Rect) : Prop :=
  r.top ≤ r.bottom ∧ r.left ≤ r.right ∧ (r.bottom - r.top + 1) * (r.right - r.left + 1) ≥ 2

instance (r : Rect) : Decidable r.wf :=
  inferInstanceAs (Decidable (_ ∧ _ ∧ _))

/-- Two regions do not overlap: their row intervals or their column intervals
are disjoint (`Grid::overlaps` negated). -/
def disjoint (a b : Rect) : Prop :=
  a.bottom < b.top ∨ b.bottom < a.top ∨ a.right < b.left ∨ b.right < a.left

instance (a b : Rect) : Decidable (disjoint a b) :=
  inferInstanceAs (Decidable (_ ∨ _ ∨ _ ∨ _))

/-- The table invariant: every region well-formed, regions pairwise disjoint. -/
def Valid (ms : List Rect) : Prop :=
  (∀ m ∈ ms, m.wf) ∧ ms.Pairwise disjoint

instance (ms : List Rect) : Decidable (Valid ms) :=
  inferInstanceAs (Decidable (_ ∧ _))

/-- The sequenced merge operation: accepted only if the result is valid. -/
def mergeOp (ms : List Rect) (r : Rect) : Option (List Rect) :=
  if Valid (r :: ms) then some (r :: ms) else none

/-- **CRDT union breaks the table invariant** (design.md §1, first row;
`naive_union_of_concurrent_merges_breaks_the_invariant` in
`crates/jade-collab-core/tests/grid.rs`): merging rows 0–1 × cols 0–1 and,
concurrently, rows 1–2 × cols 1–2 are each valid on the common ancestor, but
their union overlaps at cell (1, 1). -/
theorem crdt_union_breaks_invariant :
    ∃ (base : List Rect) (a b : Rect),
      Valid base ∧ Valid (a :: base) ∧ Valid (b :: base) ∧ ¬ Valid (a :: b :: base) :=
  ⟨[], ⟨0, 1, 0, 1⟩, ⟨1, 2, 1, 2⟩, by decide⟩

/-- **Sequenced merges stay valid**: whatever merges arrive, in whatever order,
the sequencer's state satisfies the table invariant. Instance of
`sequenced_preserves`. -/
theorem merge_sequenced_valid {ms : List Rect} (h : Valid ms) (rs : List Rect) :
    Valid (rs.foldl (step mergeOp) ms) := by
  apply sequenced_preserves mergeOp Valid _ rs ms h
  intro s r s' _ hr
  unfold mergeOp at hr
  split at hr
  · cases hr; assumption
  · cases hr

end JadeCollab.Sequencer
