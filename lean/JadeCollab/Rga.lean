/-!
# Strong eventual consistency of the RGA specification

Mirrors `rga::spec` in `crates/jade-collab-core/src/rga.rs` and §4 of
`docs/design.md`: a story is the *set* of insert operations plus the set of
deleted identifiers, and the visible document is a pure function of those
sets — a depth-first walk of the parent tree with siblings in descending
identifier order, tombstones filtered out.
-/

namespace JadeCollab.Rga

/-- `Id { lamport, replica }` from `crates/jade-collab-core/src/id.rs`. -/
structure Id where
  lamport : Nat
  replica : Nat
deriving DecidableEq, Repr

/-- One insert operation: the atom `id` with payload `value`, placed after
`parent` (`none` = story start). -/
structure Node where
  id : Id
  parent : Option Id
  value : Nat
deriving DecidableEq, Repr

/-- Sibling order: `a` comes before (or together with) `b` when
`(a.id.lamport, a.id.replica, a.value) ≥ (b.id.lamport, b.id.replica, b.value)`
lexicographically. Including `value` makes the order antisymmetric on whole
nodes with the same parent, so no uniqueness-of-ids assumption is needed. -/
def sibGe (a b : Node) : Bool :=
  decide (b.id.lamport < a.id.lamport) ||
    (decide (a.id.lamport = b.id.lamport) &&
      (decide (b.id.replica < a.id.replica) ||
        (decide (a.id.replica = b.id.replica) && decide (b.value ≤ a.value))))

theorem sibGe_trans (a b c : Node) : sibGe a b → sibGe b c → sibGe a c := by
  unfold sibGe
  simp only [Bool.or_eq_true, Bool.and_eq_true, decide_eq_true_eq]
  omega

theorem sibGe_total (a b : Node) : (sibGe a b || sibGe b a) = true := by
  unfold sibGe
  simp only [Bool.or_eq_true, Bool.and_eq_true, decide_eq_true_eq]
  omega

theorem sibGe_antisymm (a b : Node) (hp : a.parent = b.parent)
    (h₁ : sibGe a b) (h₂ : sibGe b a) : a = b := by
  unfold sibGe at h₁ h₂
  simp only [Bool.or_eq_true, Bool.and_eq_true, decide_eq_true_eq] at h₁ h₂
  obtain ⟨⟨al, ar⟩, ap, av⟩ := a
  obtain ⟨⟨bl, br⟩, bp, bv⟩ := b
  simp only at hp h₁ h₂
  have hl : al = bl := by omega
  have hr : ar = br := by omega
  have hv : av = bv := by omega
  subst hl hr hv hp
  rfl

/-- The children of `p`, in document (descending) order. -/
def children (s : List Node) (p : Option Id) : List Node :=
  (s.filter (·.parent == p)).mergeSort sibGe

/-- Fuel-bounded depth-first walk of the parent tree below `p`. With fuel
`s.length` every reachable atom is visited (each level of the tree contains a
distinct node of `s`; deeper levels only arise from cycles, which ids ordered by
Lamport clock rule out in practice). The theorems below hold for any fuel. -/
def walk : Nat → List Node → Option Id → List Node
  | 0, _, _ => []
  | n + 1, s, p => (children s p).flatMap (fun c => c :: walk n s (some c.id))

/-- The visible document: walk from the story start, drop tombstones. -/
def render (s : List Node) (deleted : List Id) : List Nat :=
  (walk s.length s none).filter (fun n => !(deleted.contains n.id)) |>.map (·.value)

/-- Receiving an insert: idempotent set insertion. -/
def deliver (s : List Node) (n : Node) : List Node :=
  if n ∈ s then s else n :: s

/-- A replica's insert set after receiving `ops` in that order. -/
def state (ops : List Node) : List Node :=
  ops.foldl deliver []

/-- Sibling order does not depend on the order the inserts arrived in
(backs the `BTreeSet` of children in `spec::linearize`). -/
theorem children_perm {s₁ s₂ : List Node} {p : Option Id} (h : s₁.Perm s₂) :
    children s₁ p = children s₂ p := by
  unfold children
  apply List.Perm.eq_of_pairwise (le := fun a b => sibGe a b = true)
  · intro a b ha hb hab hba
    have ha' := (List.mergeSort_perm _ _).mem_iff.mp ha
    have hb' := (List.mergeSort_perm _ _).mem_iff.mp hb
    simp only [List.mem_filter, beq_iff_eq] at ha' hb'
    exact sibGe_antisymm a b (ha'.2.trans hb'.2.symm) hab hba
  · exact List.pairwise_mergeSort sibGe_trans sibGe_total _
  · exact List.pairwise_mergeSort sibGe_trans sibGe_total _
  · exact ((List.mergeSort_perm _ _).trans (h.filter _)).trans
      (List.mergeSort_perm _ _).symm

/-- The depth-first walk depends only on the set of inserts, not their order. -/
theorem walk_perm {s₁ s₂ : List Node} (h : s₁.Perm s₂) :
    ∀ (k : Nat) (p : Option Id), walk k s₁ p = walk k s₂ p := by
  intro k
  induction k with
  | zero => intro p; rfl
  | succ k ih =>
    intro p
    simp only [walk, children_perm h, ih]

theorem foldl_deliver_nodup (ops : List Node) :
    ∀ s : List Node, s.Nodup → (ops.foldl deliver s).Nodup := by
  induction ops with
  | nil => intro s hs; exact hs
  | cons o ops ih =>
    intro s hs
    apply ih
    unfold deliver
    split
    · exact hs
    · exact List.nodup_cons.mpr ⟨by assumption, hs⟩

theorem foldl_deliver_mem (ops : List Node) (n : Node) :
    ∀ s : List Node, n ∈ ops.foldl deliver s ↔ n ∈ s ∨ n ∈ ops := by
  induction ops with
  | nil => intro s; simp
  | cons o ops ih =>
    intro s
    rw [List.foldl_cons, ih]
    unfold deliver
    split
    · rename_i ho
      constructor
      · rintro (h | h)
        · exact Or.inl h
        · exact Or.inr (List.mem_cons_of_mem _ h)
      · rintro (h | h)
        · exact Or.inl h
        · rcases List.mem_cons.mp h with h | h
          · exact Or.inl (h ▸ ho)
          · exact Or.inr h
    · simp only [List.mem_cons]
      constructor
      · rintro ((h | h) | h)
        · exact Or.inr (Or.inl h)
        · exact Or.inl h
        · exact Or.inr (Or.inr h)
      · rintro (h | h | h)
        · exact Or.inl (Or.inr h)
        · exact Or.inl (Or.inl h)
        · exact Or.inr h

/-- Duplicate deliveries are absorbed: a replica's insert set has no repeats. -/
theorem state_nodup (ops : List Node) : (state ops).Nodup :=
  foldl_deliver_nodup ops [] List.nodup_nil

/-- A replica's insert set holds exactly the inserts it received. -/
theorem state_mem {ops : List Node} {n : Node} : n ∈ state ops ↔ n ∈ ops := by
  unfold state
  rw [foldl_deliver_mem]
  simp

/-- Replicas that received the same inserts (any order, any duplication) hold
the same insert set up to permutation. -/
theorem state_perm {ops₁ ops₂ : List Node} (h : ∀ n, n ∈ ops₁ ↔ n ∈ ops₂) :
    (state ops₁).Perm (state ops₂) :=
  (List.perm_ext_iff_of_nodup (state_nodup ops₁) (state_nodup ops₂)).mpr
    (fun n => by rw [state_mem, state_mem]; exact h n)

/-- `render` is a function of the insert *set* and the deletion *set*
(the property `spec_is_order_and_duplicate_insensitive` in
`crates/jade-collab-core/tests/rga.rs` checks on random data). -/
theorem render_congr {s₁ s₂ : List Node} {del₁ del₂ : List Id} (hs : s₁.Perm s₂)
    (hd : ∀ i, i ∈ del₁ ↔ i ∈ del₂) : render s₁ del₁ = render s₂ del₂ := by
  unfold render
  rw [hs.length_eq, walk_perm hs]
  congr 2
  funext n
  have : del₁.contains n.id = del₂.contains n.id := by
    rw [Bool.eq_iff_iff, List.contains_iff_mem, List.contains_iff_mem]
    exact hd n.id
  rw [this]

/-- **Strong eventual consistency of the RGA spec** (design.md §4,
`rga::spec::render`): replicas that delivered the same insert operations and
the same deletions — in any order and with any duplication — show the same
document. -/
theorem converges {ops₁ ops₂ : List Node} {del₁ del₂ : List Id}
    (hops : ∀ n, n ∈ ops₁ ↔ n ∈ ops₂) (hdel : ∀ i, i ∈ del₁ ↔ i ∈ del₂) :
    render (state ops₁) del₁ = render (state ops₂) del₂ :=
  render_congr (state_perm hops) hdel

end JadeCollab.Rga
