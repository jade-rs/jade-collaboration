import JadeCollab.Rga
import JadeCollab.Lww

/-!
# A Peritext-style mark CRDT over the RGA

Mirrors `jade_collab_core::marks` (`crates/jade-collab-core/src/marks.rs`)
and the "Formatting" section of `docs/design.md` §4.

A *mark* says "attribute `key` has `value` from boundary `first` to boundary
`last`". A boundary is a side of an atom of the story's RGA (`before a`,
`after a`) or an end of the story (`start`, `stop`). Atoms are never removed
from an RGA — a deletion leaves a tombstone — so a mark's anchors never
disappear. An atom is *covered* when it lies strictly between the two
boundaries in the story's order (tombstones included). For each key, an atom
shows the value of the covering mark with the greatest identifier
(last-writer-wins, `JadeCollab.Lww`).

Which boundaries a mark gets decides how it *expands*: a mark over the
atoms `first … last` starts `before first` (typing in front of it is not
covered) and ends `before` the atom that followed `last` when the mark was
made, or at `stop` (bold, italic: typing at its end is covered), or
`after last` (a link: typing at its end is not).

What is proved:

* `effective_congr`, `converges`: an atom's formatting is a function of the
  *set* of mark operations a replica received (any order, any duplication).
* `format_converges`, `richRender_converges`: together with the RGA's own
  strong eventual consistency (`Rga.converges`), replicas that received the
  same inserts, deletions and marks show the same text with the same
  formatting. Deletions do not enter the formatting at all.
* `covers_preserved`, `effective_preserved`: atoms inserted later — anywhere,
  any number, concurrently or not — never change the formatting of the atoms
  that were there, as long as the new atoms keep the old ones' relative order
  (which the RGA does).
* `covers_in_gap`, `effective_in_gap`: an atom that lands in the gap right
  after atom `l` gets exactly the formatting of the marks spanning that gap;
  `covers_at_start` for the gap before the first atom.
* `markOver_covers_iff`, `typed_inside`, `typed_after_end`,
  `typed_before_start`: the expand rules — an atom inserted inside a mark's
  range is covered, one inserted right after its last atom is covered exactly
  when the mark expands, one inserted right before its first atom never is.
-/

namespace JadeCollab.Marks

open JadeCollab

abbrev Id := Rga.Id

/-! ## Positions in a sequence -/

/-- The index of the first occurrence of `a` in `seq`. -/
def idx : List Id → Id → Option Nat
  | [], _ => none
  | b :: l, a => if b = a then some 0 else (idx l a).map (· + 1)

theorem idx_mem : ∀ {seq : List Id} {a : Id} {i : Nat}, idx seq a = some i → a ∈ seq
  | [], _, _, h => by simp [idx] at h
  | b :: l, a, i, h => by
    unfold idx at h
    split at h
    · rename_i hb; exact hb ▸ List.mem_cons_self
    · cases hi : idx l a with
      | none => rw [hi] at h; simp at h
      | some j => exact List.mem_cons_of_mem _ (idx_mem hi)

theorem idx_of_mem : ∀ {seq : List Id} {a : Id}, a ∈ seq → ∃ i, idx seq a = some i
  | [], _, h => by cases h
  | b :: l, a, h => by
    unfold idx
    split
    · exact ⟨0, rfl⟩
    · rename_i hb
      rcases List.mem_cons.mp h with h | h
      · exact absurd h.symm hb
      · obtain ⟨j, hj⟩ := idx_of_mem h
        exact ⟨j + 1, by rw [hj]; rfl⟩

theorem idx_getElem? : ∀ {seq : List Id} {a : Id} {i : Nat}, idx seq a = some i → seq[i]? = some a
  | [], _, _, h => by simp [idx] at h
  | b :: l, a, i, h => by
    unfold idx at h
    split at h
    · rename_i hb; cases h; simp [hb]
    · cases hi : idx l a with
      | none => rw [hi] at h; simp at h
      | some j =>
        rw [hi] at h
        simp only [Option.map_some, Option.some.injEq] at h
        subst h
        simpa using idx_getElem? hi

/-- Two atoms at the same index are the same atom. -/
theorem idx_inj {seq : List Id} {a b : Id} {i : Nat} (ha : idx seq a = some i)
    (hb : idx seq b = some i) : a = b := by
  have h₁ := idx_getElem? ha
  have h₂ := idx_getElem? hb
  rw [h₁] at h₂
  exact Option.some.inj h₂

/-- `a` comes strictly before `b` in `seq` (both present). -/
def lt (seq : List Id) (a b : Id) : Bool :=
  match idx seq a, idx seq b with
  | some i, some j => decide (i < j)
  | _, _ => false

/-- `a` comes before `b` or is `b` (both present). -/
def le (seq : List Id) (a b : Id) : Bool :=
  match idx seq a, idx seq b with
  | some i, some j => decide (i ≤ j)
  | _, _ => false

theorem lt_mem {seq : List Id} {a b : Id} (h : lt seq a b = true) : a ∈ seq ∧ b ∈ seq := by
  unfold lt at h
  split at h
  · rename_i i j ha hb; exact ⟨idx_mem ha, idx_mem hb⟩
  · cases h

theorem le_mem {seq : List Id} {a b : Id} (h : le seq a b = true) : a ∈ seq ∧ b ∈ seq := by
  unfold le at h
  split at h
  · rename_i i j ha hb; exact ⟨idx_mem ha, idx_mem hb⟩
  · cases h

theorem le_iff {seq : List Id} {a b : Id} :
    le seq a b = true ↔ (a = b ∧ a ∈ seq) ∨ lt seq a b = true := by
  constructor
  · intro h
    obtain ⟨ha, hb⟩ := le_mem h
    obtain ⟨i, hi⟩ := idx_of_mem ha
    obtain ⟨j, hj⟩ := idx_of_mem hb
    unfold le at h
    unfold lt
    rw [hi, hj] at h ⊢
    simp only [decide_eq_true_eq] at h ⊢
    rcases Nat.lt_or_eq_of_le h with h | h
    · exact Or.inr h
    · subst h; exact Or.inl ⟨idx_inj hi hj, ha⟩
  · rintro (⟨rfl, ha⟩ | h)
    · obtain ⟨i, hi⟩ := idx_of_mem ha
      unfold le; rw [hi]; simp
    · obtain ⟨ha, hb⟩ := lt_mem h
      obtain ⟨i, hi⟩ := idx_of_mem ha
      obtain ⟨j, hj⟩ := idx_of_mem hb
      unfold lt at h; unfold le
      rw [hi, hj] at h ⊢
      simp only [decide_eq_true_eq] at h ⊢
      omega

/-- Two distinct atoms of `seq` are ordered one way or the other. -/
theorem lt_total {seq : List Id} {a b : Id} (ha : a ∈ seq) (hb : b ∈ seq) (hne : a ≠ b) :
    lt seq a b = true ∨ lt seq b a = true := by
  obtain ⟨i, hi⟩ := idx_of_mem ha
  obtain ⟨j, hj⟩ := idx_of_mem hb
  unfold lt
  rw [hi, hj]
  simp only [decide_eq_true_eq]
  rcases Nat.lt_trichotomy i j with h | h | h
  · exact Or.inl h
  · subst h; exact absurd (idx_inj hi hj) hne
  · exact Or.inr h

theorem lt_asymm {seq : List Id} {a b : Id} (h : lt seq a b = true) : lt seq b a = false := by
  unfold lt at h ⊢
  split at h
  · rename_i i j hi hj
    rw [hi, hj]
    simp only [decide_eq_true_eq, decide_eq_false_iff_not] at h ⊢
    omega
  · cases h

theorem lt_irrefl (seq : List Id) (a : Id) : lt seq a a = false := by
  unfold lt
  split
  · rename_i i j hi hj
    rw [hi] at hj
    cases hj
    simp
  · rfl

theorem lt_of_le_of_lt {seq : List Id} {a b c : Id} (h₁ : le seq a b = true)
    (h₂ : lt seq b c = true) : lt seq a c = true := by
  unfold le at h₁
  unfold lt at h₂ ⊢
  split at h₁
  · rename_i i j hi hj
    split at h₂
    · rename_i j' k hj' hk
      rw [hj] at hj'
      cases hj'
      rw [hi, hk]
      simp only [decide_eq_true_eq] at h₁ h₂ ⊢
      omega
    · cases h₂
  · cases h₁

theorem lt_of_lt_of_le {seq : List Id} {a b c : Id} (h₁ : lt seq a b = true)
    (h₂ : le seq b c = true) : lt seq a c = true := by
  unfold lt at h₁ ⊢
  unfold le at h₂
  split at h₁
  · rename_i i j hi hj
    split at h₂
    · rename_i j' k hj' hk
      rw [hj] at hj'
      cases hj'
      rw [hi, hk]
      simp only [decide_eq_true_eq] at h₁ h₂ ⊢
      omega
    · cases h₂
  · cases h₁

theorem le_of_lt {seq : List Id} {a b : Id} (h : lt seq a b = true) : le seq a b = true :=
  le_iff.mpr (Or.inr h)

theorem le_refl {seq : List Id} {a : Id} (h : a ∈ seq) : le seq a a = true :=
  le_iff.mpr (Or.inl ⟨rfl, h⟩)


/-! ## Marks -/

/-- Where a mark starts or ends (`Boundary` in `marks.rs`). -/
inductive Boundary where
  /-- Before every atom of the story. -/
  | start
  /-- Right before atom `a`. -/
  | before (a : Id)
  /-- Right after atom `a`. -/
  | after (a : Id)
  /-- After every atom of the story. -/
  | stop
deriving DecidableEq, Repr

/-- One mark operation (`Mark` in `marks.rs`): attribute `key` gets `value`
between `first` and `last`. `value` is opaque here (the Rust value, or "back to
the base formatting", is any payload); it is the last tiebreak of the
last-writer-wins order, as in `Lww.lean`, so no uniqueness of identifiers is
assumed. -/
structure Mark where
  id : Id
  key : Nat
  value : Nat
  first : Boundary
  last : Boundary
deriving DecidableEq, Repr

/-- Whether a mark starting at `b` starts before atom `x`. -/
def startsBefore (seq : List Id) (b : Boundary) (x : Id) : Bool :=
  match b with
  | .start => true
  | .before a => le seq a x
  | .after a => lt seq a x
  | .stop => false

/-- Whether a mark ending at `b` ends after atom `x`. -/
def endsAfter (seq : List Id) (b : Boundary) (x : Id) : Bool :=
  match b with
  | .start => false
  | .before a => lt seq x a
  | .after a => le seq x a
  | .stop => true

/-- Atom `x` of `seq` lies strictly between the mark's boundaries. A mark one
of whose anchor atoms is missing covers nothing. -/
def covers (seq : List Id) (m : Mark) (x : Id) : Bool :=
  decide (x ∈ seq) && startsBefore seq m.first x && endsAfter seq m.last x

/-- A mark as a last-writer-wins write: stamp, then value. -/
def toWrite (m : Mark) : Lww.Write := (m.id.lamport, m.id.replica, m.value)

/-- What atom `x` shows for attribute `k`: the newest covering mark's write,
`none` when no mark covers it (the atom keeps its base formatting). -/
def effective (seq : List Id) (ms : List Mark) (x : Id) (k : Nat) : Lww.Reg :=
  ((ms.filter (fun m => m.key == k && covers seq m x)).map toWrite).foldl Lww.write none

/-- `effective` depends only on the *set* of marks. -/
theorem effective_congr {seq : List Id} {ms₁ ms₂ : List Mark} (h : ∀ m, m ∈ ms₁ ↔ m ∈ ms₂)
    (x : Id) (k : Nat) : effective seq ms₁ x k = effective seq ms₂ x k := by
  unfold effective
  apply Lww.converges
  intro w
  simp only [List.mem_map, List.mem_filter, h]

/-! ## Delivering marks -/

/-- Receiving an operation: idempotent set insertion. -/
def deliver {α : Type} [DecidableEq α] (s : List α) (a : α) : List α :=
  if a ∈ s then s else a :: s

theorem foldl_deliver_mem {α : Type} [DecidableEq α] (ops : List α) (a : α) :
    ∀ s : List α, a ∈ ops.foldl deliver s ↔ a ∈ s ∨ a ∈ ops := by
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

/-- A replica's mark set after receiving `ops` in that order. -/
def markState (ops : List Mark) : List Mark :=
  ops.foldl deliver []

theorem markState_mem {ops : List Mark} {m : Mark} : m ∈ markState ops ↔ m ∈ ops := by
  unfold markState
  rw [foldl_deliver_mem]
  simp

/-- **Convergence of marks**: replicas that received the same mark operations,
in any order and with any duplication, give every atom of a sequence the same
formatting. -/
theorem converges {ops₁ ops₂ : List Mark} (h : ∀ m, m ∈ ops₁ ↔ m ∈ ops₂) (seq : List Id)
    (x : Id) (k : Nat) :
    effective seq (markState ops₁) x k = effective seq (markState ops₂) x k :=
  effective_congr (fun m => by rw [markState_mem, markState_mem]; exact h m) x k

/-- Applying two mark operations in either order gives the same formatting. -/
theorem deliver_comm (s : List Mark) (a b : Mark) (seq : List Id) (x : Id) (k : Nat) :
    effective seq (deliver (deliver s a) b) x k = effective seq (deliver (deliver s b) a) x k := by
  apply effective_congr
  intro m
  have h := fun (t : List Mark) (c : Mark) => foldl_deliver_mem [c] m t
  simp only [List.foldl_cons, List.foldl_nil] at h
  rw [h, h, h, h, List.mem_singleton, List.mem_singleton]
  constructor <;> rintro ((h | h) | h) <;> simp_all

/-! ## Marks over the RGA -/

/-- A story's order: every atom ever inserted, tombstones included. -/
def order (s : List Rga.Node) : List Id :=
  (Rga.walk s.length s none).map (·.id)

theorem order_perm {s₁ s₂ : List Rga.Node} (h : s₁.Perm s₂) : order s₁ = order s₂ := by
  unfold order
  rw [h.length_eq, Rga.walk_perm h]

/-- The formatting of atom `x` for attribute `k` on a replica that received
the inserts `ops` and the marks `marks`. Deletions play no part: a tombstone
still anchors marks and still takes part in the order. -/
def format (ops : List Rga.Node) (marks : List Mark) (x : Id) (k : Nat) : Lww.Reg :=
  effective (order (Rga.state ops)) (markState marks) x k

/-- **Strong eventual consistency of formatting**: replicas that received the
same inserts and the same marks — in any order, with any duplication, and
interleaved with each other in any way — format every atom alike. -/
theorem format_converges {ops₁ ops₂ : List Rga.Node} {ms₁ ms₂ : List Mark}
    (hops : ∀ n, n ∈ ops₁ ↔ n ∈ ops₂) (hms : ∀ m, m ∈ ms₁ ↔ m ∈ ms₂) (x : Id) (k : Nat) :
    format ops₁ ms₁ x k = format ops₂ ms₂ x k := by
  unfold format
  rw [order_perm (Rga.state_perm hops)]
  exact converges hms _ x k

/-- The visible document with formatting: each visible atom's value and what
it shows for each attribute of `keys`. -/
def richRender (ops : List Rga.Node) (del : List Id) (marks : List Mark) (keys : List Nat) :
    List (Nat × List Lww.Reg) :=
  let s := Rga.state ops
  ((Rga.walk s.length s none).filter (fun n => !(del.contains n.id))).map
    (fun n => (n.value, keys.map (format ops marks n.id)))

/-- **Strong eventual consistency of rich text**: the same inserts, deletions
and marks, delivered in any order with any duplication, show the same text
with the same formatting. -/
theorem richRender_converges {ops₁ ops₂ : List Rga.Node} {del₁ del₂ : List Id}
    {ms₁ ms₂ : List Mark} (hops : ∀ n, n ∈ ops₁ ↔ n ∈ ops₂) (hdel : ∀ i, i ∈ del₁ ↔ i ∈ del₂)
    (hms : ∀ m, m ∈ ms₁ ↔ m ∈ ms₂) (keys : List Nat) :
    richRender ops₁ del₁ ms₁ keys = richRender ops₂ del₂ ms₂ keys := by
  have hs := Rga.state_perm hops
  unfold richRender
  simp only
  rw [hs.length_eq, Rga.walk_perm hs]
  have hd : (fun n : Rga.Node => !(del₁.contains n.id)) = (fun n => !(del₂.contains n.id)) := by
    funext n
    have : del₁.contains n.id = del₂.contains n.id := by
      rw [Bool.eq_iff_iff, List.contains_iff_mem, List.contains_iff_mem]
      exact hdel n.id
    rw [this]
  rw [hd]
  congr 1
  funext n
  congr 1
  apply List.map_congr_left
  intro k _
  exact format_converges hops hms n.id k


/-! ## Inserting atoms never reformats old ones -/

theorem lt_idx {seq : List Id} {a b : Id} {i j : Nat} (ha : idx seq a = some i)
    (hb : idx seq b = some j) : lt seq a b = decide (i < j) := by
  unfold lt; rw [ha, hb]

theorem le_idx {seq : List Id} {a b : Id} {i j : Nat} (ha : idx seq a = some i)
    (hb : idx seq b = some j) : le seq a b = decide (i ≤ j) := by
  unfold le; rw [ha, hb]

theorem le_eq_lt_of_ne {seq : List Id} {a b : Id} (hne : a ≠ b) : le seq a b = lt seq a b := by
  rw [Bool.eq_iff_iff, le_iff]
  constructor
  · rintro (⟨rfl, _⟩ | h)
    · exact absurd rfl hne
    · exact h
  · exact Or.inr

/-- Between two distinct atoms of `seq`, exactly one comes first. -/
theorem lt_eq_not_lt {seq : List Id} {a b : Id} (ha : a ∈ seq) (hb : b ∈ seq) (hne : a ≠ b) :
    lt seq a b = !(lt seq b a) := by
  rcases lt_total ha hb hne with h | h
  · rw [h, lt_asymm h]; rfl
  · rw [h, lt_asymm h]; rfl

theorem lt_eq_not_le {seq : List Id} {a b : Id} (ha : a ∈ seq) (hb : b ∈ seq) :
    lt seq a b = !(le seq b a) := by
  obtain ⟨i, hi⟩ := idx_of_mem ha
  obtain ⟨j, hj⟩ := idx_of_mem hb
  rw [lt_idx hi hj, le_idx hj hi]
  by_cases h : i < j
  · simp [h, Nat.not_le.mpr h]
  · simp [h, Nat.le_of_not_lt h]

/-- `seq'` extends `seq`: it holds every atom of `seq`, in the same relative
order. The RGA guarantees this for every insert (an atom is integrated into
the walk without moving any other). -/
structure Extends (seq seq' : List Id) : Prop where
  mem : ∀ a, a ∈ seq → a ∈ seq'
  lt_eq : ∀ a b, a ∈ seq → b ∈ seq → lt seq' a b = lt seq a b

theorem Extends.le_eq {seq seq' : List Id} (h : Extends seq seq') {a b : Id} (ha : a ∈ seq)
    (hb : b ∈ seq) : le seq' a b = le seq a b := by
  rw [Bool.eq_iff_iff, le_iff, le_iff, h.lt_eq a b ha hb]
  constructor
  · rintro (⟨rfl, _⟩ | h)
    · exact Or.inl ⟨rfl, ha⟩
    · exact Or.inr h
  · rintro (⟨rfl, _⟩ | h')
    · exact Or.inl ⟨rfl, h.mem _ ha⟩
    · exact Or.inr h'

/-- The atom a boundary is anchored to, if any. -/
def Boundary.atom : Boundary → Option Id
  | .before a => some a
  | .after a => some a
  | _ => none

/-- Every atom the mark is anchored to is in `seq` (marks are only made on
atoms that exist; a tombstone still counts). -/
def Anchored (seq : List Id) (m : Mark) : Prop :=
  (∀ a, m.first.atom = some a → a ∈ seq) ∧ (∀ a, m.last.atom = some a → a ∈ seq)

/-- Inserting atoms never changes which old atoms a mark covers. -/
theorem covers_preserved {seq seq' : List Id} (h : Extends seq seq') {m : Mark}
    (hm : Anchored seq m) {x : Id} (hx : x ∈ seq) : covers seq' m x = covers seq m x := by
  unfold covers
  have h₁ : startsBefore seq' m.first x = startsBefore seq m.first x := by
    unfold startsBefore
    match hf : m.first with
    | .start => rfl
    | .stop => rfl
    | .before a => exact h.le_eq (hm.1 a (by rw [hf]; rfl)) hx
    | .after a => exact h.lt_eq a x (hm.1 a (by rw [hf]; rfl)) hx
  have h₂ : endsAfter seq' m.last x = endsAfter seq m.last x := by
    unfold endsAfter
    match hl : m.last with
    | .start => rfl
    | .stop => rfl
    | .before a => exact h.lt_eq x a hx (hm.2 a (by rw [hl]; rfl))
    | .after a => exact h.le_eq hx (hm.2 a (by rw [hl]; rfl))
  rw [h₁, h₂]
  simp [hx, h.mem x hx]

/-- **Inserting text never reformats existing text**: in a sequence extended
by any number of new atoms, every old atom keeps its formatting. -/
theorem effective_preserved {seq seq' : List Id} (h : Extends seq seq') {ms : List Mark}
    (hms : ∀ m, m ∈ ms → Anchored seq m) {x : Id} (hx : x ∈ seq) (k : Nat) :
    effective seq' ms x k = effective seq ms x k := by
  unfold effective
  congr 2
  apply List.filter_congr
  intro m hm
  rw [covers_preserved h (hms m hm) hx]

/-! ## What a new atom gets -/

/-- In `seq'`, the new atom `x` sits in the gap right after the old atom `l`:
after every old atom up to `l`, before every later one. Any number of atoms
inserted concurrently into that gap each satisfy this. -/
structure InGap (seq seq' : List Id) (l x : Id) : Prop where
  ext : Extends seq seq'
  l_mem : l ∈ seq
  fresh : x ∉ seq
  mem : x ∈ seq'
  after : ∀ a, a ∈ seq → lt seq' a x = le seq a l

/-- The mark spans the gap right after `l`. -/
def spansGapAfter (seq : List Id) (m : Mark) (l : Id) : Bool :=
  (match m.first with
   | .start => true
   | .before a => le seq a l
   | .after a => le seq a l
   | .stop => false) &&
  (match m.last with
   | .start => false
   | .before b => lt seq l b
   | .after b => lt seq l b
   | .stop => true)

theorem InGap.lt_x_eq {seq seq' : List Id} {l x : Id} (g : InGap seq seq' l x) {b : Id}
    (hb : b ∈ seq) : lt seq' x b = lt seq l b := by
  have hne : x ≠ b := fun e => g.fresh (e ▸ hb)
  rw [lt_eq_not_lt g.mem (g.ext.mem b hb) hne, g.after b hb, lt_eq_not_le g.l_mem hb]

/-- **A new atom gets the formatting of the gap it lands in**: it is covered
by exactly the marks spanning the gap after `l`. -/
theorem covers_in_gap {seq seq' : List Id} {l x : Id} (g : InGap seq seq' l x) {m : Mark}
    (hm : Anchored seq m) : covers seq' m x = spansGapAfter seq m l := by
  unfold covers spansGapAfter
  have h₁ : startsBefore seq' m.first x =
      (match m.first with
       | .start => true
       | .before a => le seq a l
       | .after a => le seq a l
       | .stop => false) := by
    unfold startsBefore
    match hf : m.first with
    | .start => rfl
    | .stop => rfl
    | .before a =>
      have ha := hm.1 a (by rw [hf]; rfl)
      have hne : a ≠ x := fun e => g.fresh (e ▸ ha)
      simp only
      rw [le_eq_lt_of_ne hne, g.after a ha]
    | .after a =>
      have ha := hm.1 a (by rw [hf]; rfl)
      simp only
      rw [g.after a ha]
  have h₂ : endsAfter seq' m.last x =
      (match m.last with
       | .start => false
       | .before b => lt seq l b
       | .after b => lt seq l b
       | .stop => true) := by
    unfold endsAfter
    match hl : m.last with
    | .start => rfl
    | .stop => rfl
    | .before b =>
      have hb := hm.2 b (by rw [hl]; rfl)
      simp only
      rw [g.lt_x_eq hb]
    | .after b =>
      have hb := hm.2 b (by rw [hl]; rfl)
      have hne : x ≠ b := fun e => g.fresh (e ▸ hb)
      simp only
      rw [le_eq_lt_of_ne hne, g.lt_x_eq hb]
  rw [h₁, h₂]
  simp [g.mem]

theorem effective_in_gap {seq seq' : List Id} {l x : Id} (g : InGap seq seq' l x)
    {ms : List Mark} (hms : ∀ m, m ∈ ms → Anchored seq m) (k : Nat) :
    effective seq' ms x k =
      ((ms.filter (fun m => m.key == k && spansGapAfter seq m l)).map toWrite).foldl
        Lww.write none := by
  unfold effective
  congr 2
  apply List.filter_congr
  intro m hm
  rw [covers_in_gap g (hms m hm)]

/-- In `seq'`, the new atom `x` comes before every old atom. -/
structure AtStart (seq seq' : List Id) (x : Id) : Prop where
  ext : Extends seq seq'
  fresh : x ∉ seq
  mem : x ∈ seq'
  first : ∀ a, a ∈ seq → lt seq' x a = true

/-- An atom inserted before everything is covered only by marks that start
at the story's start. -/
theorem covers_at_start {seq seq' : List Id} {x : Id} (g : AtStart seq seq' x) {m : Mark}
    (hm : Anchored seq m) : covers seq' m x = (m.first == .start && m.last != .start) := by
  unfold covers
  have h₁ : startsBefore seq' m.first x = (m.first == .start) := by
    unfold startsBefore
    match hf : m.first with
    | .start => rfl
    | .stop => rfl
    | .before a =>
      have ha := hm.1 a (by rw [hf]; rfl)
      have hne : a ≠ x := fun e => g.fresh (e ▸ ha)
      simp only
      rw [le_eq_lt_of_ne hne, lt_asymm (g.first a ha)]
      rfl
    | .after a =>
      have ha := hm.1 a (by rw [hf]; rfl)
      simp only
      rw [lt_asymm (g.first a ha)]
      rfl
  have h₂ : endsAfter seq' m.last x = (m.last != .start) := by
    unfold endsAfter
    match hl : m.last with
    | .start => rfl
    | .stop => rfl
    | .before b => simp only; rw [g.first b (hm.2 b (by rw [hl]; rfl))]; rfl
    | .after b => simp only; rw [le_of_lt (g.first b (hm.2 b (by rw [hl]; rfl)))]; rfl
  rw [h₁, h₂]
  simp [g.mem]

/-! ## Expand rules -/

/-- The atom right after `a` in `seq`. -/
def succ : List Id → Id → Option Id
  | a :: b :: rest, x => if a = x then some b else succ (b :: rest) x
  | _, _ => none

theorem succ_spec : ∀ {seq : List Id} {a n : Id}, succ seq a = some n →
    ∃ i, idx seq a = some i ∧ seq[i + 1]? = some n
  | [], _, _, h => by simp [succ] at h
  | [_], _, _, h => by simp [succ] at h
  | c :: b :: rest, a, n, h => by
    unfold succ at h
    split at h
    · rename_i hc
      cases h
      exact ⟨0, by simp [idx, hc], rfl⟩
    · rename_i hc
      obtain ⟨i, hi, hn⟩ := succ_spec h
      refine ⟨i + 1, ?_, by simpa using hn⟩
      rw [idx]
      simp only [hc, ↓reduceIte]
      rw [hi]
      rfl

theorem succ_none : ∀ {seq : List Id} {a : Id} {i : Nat}, succ seq a = none →
    idx seq a = some i → seq[i + 1]? = none
  | [], _, _, _, h => by simp [idx] at h
  | [c], a, i, _, h => by
    unfold idx at h
    split at h
    · cases h; rfl
    · simp [idx] at h
  | c :: b :: rest, a, i, hs, h => by
    unfold succ at hs
    split at hs
    · cases hs
    · rename_i hc
      unfold idx at h
      simp only [hc, ↓reduceIte] at h
      cases hi : idx (b :: rest) a with
      | none => rw [hi] at h; cases h
      | some j =>
        rw [hi] at h
        cases h
        simpa using succ_none hs hi

theorem idx_of_getElem? : ∀ {seq : List Id} {a : Id} {i : Nat}, seq.Nodup →
    seq[i]? = some a → idx seq a = some i
  | [], _, _, _, h => by simp at h
  | b :: l, a, 0, _, h => by
    simp only [List.getElem?_cons_zero, Option.some.injEq] at h
    simp [idx, h]
  | b :: l, a, i + 1, hnd, h => by
    simp only [List.getElem?_cons_succ] at h
    have hal : a ∈ l := List.mem_of_getElem? h
    have hba : b ≠ a := fun e => (List.nodup_cons.mp hnd).1 (e ▸ hal)
    rw [idx]
    simp only [hba, ↓reduceIte]
    rw [idx_of_getElem? (List.nodup_cons.mp hnd).2 h]
    rfl

/-- The mark the intent API makes over the atoms `first … last` of `seq`
(`anchors` in `marks.rs`): it starts right before `first`; it ends right
before the atom that follows `last` (or at the story's end) when its
attribute expands, right after `last` when it does not. -/
def markOver (seq : List Id) (id : Id) (key value : Nat) (first last : Id) (expand : Bool) :
    Mark :=
  { id, key, value, first := .before first,
    last := if expand then
        (match succ seq last with
         | some n => .before n
         | none => .stop)
      else .after last }

theorem markOver_anchored {seq : List Id} {id : Id} {key value : Nat} {first last : Id}
    {expand : Bool} (hf : first ∈ seq) (hl : last ∈ seq) :
    Anchored seq (markOver seq id key value first last expand) := by
  refine ⟨fun a h => ?_, fun a h => ?_⟩
  · simp only [markOver, Boundary.atom, Option.some.injEq] at h
    exact h ▸ hf
  · unfold markOver at h
    cases expand with
    | false =>
      simp only [Bool.false_eq_true, ite_false, Boundary.atom, Option.some.injEq] at h
      exact h ▸ hl
    | true =>
      simp only [ite_true] at h
      cases hs : succ seq last with
      | none => rw [hs] at h; cases h
      | some n =>
        rw [hs] at h
        simp only [Boundary.atom, Option.some.injEq] at h
        subst h
        obtain ⟨i, _, hn⟩ := succ_spec hs
        exact List.mem_of_getElem? hn

/-- The mark covers exactly `first … last` of the sequence it was made on,
whether it expands or not. -/
theorem markOver_covers_iff {seq : List Id} (hnd : seq.Nodup) {id : Id} {key value : Nat}
    {first last : Id} {expand : Bool} {y : Id} (hl : last ∈ seq) (hy : y ∈ seq) :
    covers seq (markOver seq id key value first last expand) y =
      (le seq first y && le seq y last) := by
  unfold covers
  have hend : endsAfter seq (markOver seq id key value first last expand).last y =
      le seq y last := by
    unfold markOver
    cases expand with
    | false => rfl
    | true =>
      simp only [ite_true]
      obtain ⟨i, hi⟩ := idx_of_mem hl
      obtain ⟨j, hj⟩ := idx_of_mem hy
      cases hs : succ seq last with
      | none =>
        have hlast := succ_none hs hi
        have hjlen : j < seq.length := by
          have := idx_getElem? hj
          exact (List.getElem?_eq_some_iff.mp this).1
        have hilen : seq.length ≤ i + 1 := List.getElem?_eq_none_iff.mp hlast
        unfold endsAfter
        rw [le_idx hj hi]
        simp only at *
        exact (decide_eq_true (by omega)).symm
      | some n =>
        obtain ⟨i', hi', hn⟩ := succ_spec hs
        rw [hi] at hi'
        cases hi'
        have hidx := idx_of_getElem? hnd hn
        unfold endsAfter
        simp only
        rw [lt_idx hj hidx, le_idx hj hi]
        by_cases h : j ≤ i
        · simp [h, Nat.lt_succ_of_le h]
        · simp [h]; omega
  rw [hend]
  simp [hy, markOver, startsBefore]

/-- **Typing inside a mark's range is covered**, whether the mark expands or
not, and whoever's insert it is. -/
theorem typed_inside {seq seq' : List Id} (hnd : seq.Nodup) {id : Id} {key value : Nat}
    {first last y x : Id} {expand : Bool} (hf : first ∈ seq) (hl : last ∈ seq)
    (g : InGap seq seq' y x) (h₁ : le seq first y = true) (h₂ : lt seq y last = true) :
    covers seq' (markOver seq id key value first last expand) x = true := by
  rw [covers_in_gap g (markOver_anchored hf hl)]
  unfold spansGapAfter markOver
  simp only [h₁, Bool.true_and]
  cases expand with
  | false => exact h₂
  | true =>
    simp only [ite_true]
    cases hs : succ seq last with
    | none => rfl
    | some n =>
      simp only
      obtain ⟨i, hi, hn⟩ := succ_spec hs
      have hidx := idx_of_getElem? hnd hn
      exact lt_of_lt_of_le h₂ (le_of_lt (by rw [lt_idx hi hidx]; simp))

/-- **Typing right after a mark's last atom is covered exactly when the mark
expands** (bold: yes; a link: no) — in any delivery order, since the
formatting is a function of the operation sets. -/
theorem typed_after_end {seq seq' : List Id} (hnd : seq.Nodup) {id : Id} {key value : Nat}
    {first last x : Id} {expand : Bool} (hf : first ∈ seq) (g : InGap seq seq' last x)
    (h : le seq first last = true) :
    covers seq' (markOver seq id key value first last expand) x = expand := by
  rw [covers_in_gap g (markOver_anchored hf g.l_mem)]
  unfold spansGapAfter markOver
  simp only [h, Bool.true_and]
  cases expand with
  | false => exact lt_irrefl seq last
  | true =>
    simp only [ite_true]
    cases hs : succ seq last with
    | none => rfl
    | some n =>
      simp only
      obtain ⟨i, hi, hn⟩ := succ_spec hs
      rw [lt_idx hi (idx_of_getElem? hnd hn)]
      simp

/-- **Typing right before a mark's first atom is never covered**, at the
story's start or after the atom `p` that precedes `first`. -/
theorem typed_before_start {seq seq' : List Id} (hnd : seq.Nodup) {id : Id} {key value : Nat}
    {p first last x : Id} {expand : Bool} (hl : last ∈ seq) (hp : succ seq p = some first)
    (g : InGap seq seq' p x) :
    covers seq' (markOver seq id key value first last expand) x = false := by
  obtain ⟨i, hi, hn⟩ := succ_spec hp
  have hf : first ∈ seq := List.mem_of_getElem? hn
  rw [covers_in_gap g (markOver_anchored hf hl)]
  unfold spansGapAfter markOver
  simp only
  rw [le_idx (idx_of_getElem? hnd hn) hi]
  simp only [Bool.and_eq_false_iff, decide_eq_false_iff_not]
  exact Or.inl (by omega)

theorem typed_at_story_start {seq seq' : List Id} {id : Id} {key value : Nat}
    {first last x : Id} {expand : Bool} (hf : first ∈ seq) (hl : last ∈ seq)
    (g : AtStart seq seq' x) :
    covers seq' (markOver seq id key value first last expand) x = false := by
  rw [covers_at_start g (markOver_anchored hf hl)]
  simp [markOver]

/-! ## Examples (`crates/jade-collab-core/tests/marks.rs`) -/

private def i (n : Nat) : Id := ⟨n, 1⟩

/-- "abc", bold over "ab" (expanding) and a link over "ab" (not); then "x"
typed after "b": bold covers it, the link does not. -/
example :
    let seq := [i 1, i 2, i 3]
    let seq' := [i 1, i 2, i 4, i 3]
    let bold := markOver seq ⟨10, 1⟩ 0 1 (i 1) (i 2) true
    let link := markOver seq ⟨11, 1⟩ 1 1 (i 1) (i 2) false
    covers seq' bold (i 4) = true ∧ covers seq' link (i 4) = false ∧
      covers seq' bold (i 3) = false := by decide

/-- A newer mark of the same attribute wins where both cover. -/
example :
    let seq := [i 1, i 2, i 3]
    let on := markOver seq ⟨10, 1⟩ 0 1 (i 1) (i 3) true
    let off := markOver seq ⟨11, 2⟩ 0 0 (i 2) (i 2) true
    effective seq [on, off] (i 2) 0 = some (11, 2, 0) ∧
      effective seq [off, on] (i 1) 0 = some (10, 1, 1) := by decide

end JadeCollab.Marks
