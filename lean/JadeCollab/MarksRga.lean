import JadeCollab.Marks
import JadeCollab.RgaWalk

/-!
# The RGA's part of the mark theorems, proved

`Marks.lean` states the expand rules and "inserting never reformats" over any
two sequences related by `Extends` / `InGap` / `AtStart`. Here those
hypotheses are proved from `Rga.walk` (`RgaWalk.lean`), for every pair of
states the RGA can reach, so the mark theorems hold unconditionally for RGA
histories:

* `extends_order`: a later state (`Later`: both valid, the earlier one's
  inserts included in the later one's) keeps every atom of the earlier one, in
  the same relative order. No other condition: any number of inserts,
  concurrent or not, anywhere.
* `inGap_order`, `atStart_order`: an atom typed after `l` (at the story start)
  lands in the gap right after `l` (before every old atom) **provided it is
  newer than every atom already typed after `l`** (`Typed`) — true when its
  typist had seen them, as Lamport clocks give. Atoms inserted concurrently
  into the same gap do not matter.
* `concurrent_typing_lands_after_unseen_sibling`: the proviso is needed. An
  atom typed after `l` by a replica that had not seen an atom typed after `l`
  with a greater id lands after that atom and its descendants, so it is not
  in the gap after `l`; typed after the end of an expanding mark, it is not
  covered.
* `effective_preserved_rga`, `format_preserved`, `effective_in_gap_rga`,
  `typed_inside_rga`, `typed_after_end_rga`, `typed_before_start_rga`,
  `typed_at_story_start_rga`: the mark theorems with the hypotheses
  discharged.
-/

namespace JadeCollab.Marks

open JadeCollab

/-! ## Positions in duplicate-free lists -/

theorem idx_eq_none {seq : List Id} {a : Id} (h : a ∉ seq) : idx seq a = none := by
  cases hi : idx seq a with
  | none => rfl
  | some i => exact absurd (idx_mem hi) h

theorem idx_cons_self (c : Id) (seq : List Id) : idx (c :: seq) c = some 0 := by
  simp [idx]

theorem idx_cons_ne {c a : Id} (seq : List Id) (h : c ≠ a) :
    idx (c :: seq) a = (idx seq a).map (· + 1) := by
  simp [idx, h]

theorem lt_cons_ne {c a b : Id} {seq : List Id} (ha : c ≠ a) (hb : c ≠ b) :
    lt (c :: seq) a b = lt seq a b := by
  unfold lt
  rw [idx_cons_ne seq ha, idx_cons_ne seq hb]
  cases idx seq a <;> cases idx seq b <;> simp

theorem lt_cons_self {c b : Id} {seq : List Id} (hb : c ≠ b) :
    lt (c :: seq) c b = decide (b ∈ seq) := by
  unfold lt
  rw [idx_cons_self, idx_cons_ne seq hb]
  by_cases h : b ∈ seq
  · obtain ⟨j, hj⟩ := idx_of_mem h
    simp [hj, h]
  · simp [idx_eq_none h, h]

theorem lt_cons_to_self {c a : Id} {seq : List Id} : lt (c :: seq) a c = false := by
  unfold lt
  rw [idx_cons_self]
  cases idx (c :: seq) a <;> simp

theorem le_cons_ne {c a b : Id} {seq : List Id} (ha : c ≠ a) (hb : c ≠ b) :
    le (c :: seq) a b = le seq a b := by
  rw [Bool.eq_iff_iff, le_iff, le_iff, lt_cons_ne ha hb]
  have : a ∈ c :: seq ↔ a ∈ seq :=
    ⟨fun h => (List.mem_cons.mp h).resolve_left (Ne.symm ha), List.mem_cons_of_mem _⟩
  rw [this]

theorem le_cons_to_self {c a : Id} {seq : List Id} (ha : a ≠ c) : le (c :: seq) a c = false := by
  cases h : le (c :: seq) a c
  · rfl
  · rcases le_iff.mp h with ⟨e, _⟩ | h
    · exact absurd e ha
    · rw [lt_cons_to_self] at h; cases h

/-- In a duplicate-free list, the order of two elements of a sublist is their
order in the sublist. -/
theorem lt_sublist {M M' : List Id} (h : M.Sublist M') (hnd : M'.Nodup) {a b : Id}
    (ha : a ∈ M) (hb : b ∈ M) : lt M' a b = lt M a b := by
  induction h with
  | slnil => cases ha
  | cons c h ih =>
    have hnd' := List.nodup_cons.mp hnd
    have hac : c ≠ a := fun e => hnd'.1 (e ▸ h.subset ha)
    have hbc : c ≠ b := fun e => hnd'.1 (e ▸ h.subset hb)
    rw [lt_cons_ne hac hbc, ih hnd'.2 ha hb]
  | cons_cons c h ih =>
    have hnd' := List.nodup_cons.mp hnd
    by_cases hac : c = a
    · subst hac
      by_cases hbc : c = b
      · subst hbc; rw [lt_irrefl, lt_irrefl]
      · have hbM := (List.mem_cons.mp hb).resolve_left (Ne.symm hbc)
        rw [lt_cons_self hbc, lt_cons_self hbc]
        simp [hbM, h.subset hbM]
    · by_cases hbc : c = b
      · subst hbc; rw [lt_cons_to_self, lt_cons_to_self]
      · rw [lt_cons_ne hac hbc, lt_cons_ne hac hbc]
        exact ih hnd'.2 ((List.mem_cons.mp ha).resolve_left (Ne.symm hac))
          ((List.mem_cons.mp hb).resolve_left (Ne.symm hbc))

theorem extends_of_sublist {M M' : List Id} (h : M.Sublist M') (hnd : M'.Nodup) :
    Extends M M' :=
  ⟨fun _ ha => h.subset ha, fun _ _ ha hb => lt_sublist h hnd ha hb⟩

/-- `M` with `x` right after `l`. -/
def insertIdAfter (l x : Id) (M : List Id) : List Id :=
  M.flatMap (fun i => if i = l then [i, x] else [i])

theorem mem_insertIdAfter {l x a : Id} {M : List Id} (ha : a ∈ M) : a ∈ insertIdAfter l x M :=
  List.mem_flatMap.mpr ⟨a, ha, by split <;> simp⟩

theorem mem_insertIdAfter_self {l x : Id} {M : List Id} (hl : l ∈ M) : x ∈ insertIdAfter l x M :=
  List.mem_flatMap.mpr ⟨l, hl, by simp⟩

/-- In `M` with `x` right after `l`, an old atom comes before `x` exactly when
it is `l` or comes before `l`. -/
theorem lt_insertIdAfter {l x : Id} : ∀ {M : List Id}, M.Nodup → l ∈ M → x ∉ M →
    ∀ {a : Id}, a ∈ M → lt (insertIdAfter l x M) a x = le M a l
  | [], _, hl, _, _, _ => by cases hl
  | c :: M, hnd, hl, hx, a, ha => by
    have hnd' := List.nodup_cons.mp hnd
    have hxc : c ≠ x := fun e => hx (e ▸ List.mem_cons_self)
    have hxM : x ∉ M := fun h => hx (List.mem_cons_of_mem _ h)
    have hax : a ≠ x := fun e => hx (e ▸ ha)
    have hcons : insertIdAfter l x (c :: M) =
        (if c = l then [c, x] else [c]) ++ insertIdAfter l x M := List.flatMap_cons
    rw [hcons]
    by_cases hcl : c = l
    · subst hcl
      rw [ite_eq_left rfl]
      show lt (c :: x :: insertIdAfter c x M) a x = le (c :: M) a c
      by_cases hac : c = a
      · subst hac
        rw [lt_cons_self hxc, le_refl List.mem_cons_self]
        simp
      · rw [lt_cons_ne hac hxc, lt_cons_to_self, le_cons_to_self (Ne.symm hac)]
    · rw [ite_eq_right hcl]
      show lt (c :: insertIdAfter l x M) a x = le (c :: M) a l
      have hlM : l ∈ M := (List.mem_cons.mp hl).resolve_left (Ne.symm hcl)
      by_cases hac : c = a
      · subst hac
        rw [lt_cons_self hxc, le_of_lt (by rw [lt_cons_self hcl]; simp [hlM])]
        simp [mem_insertIdAfter_self hlM]
      · rw [lt_cons_ne hac hxc, le_cons_ne hac hcl]
        exact lt_insertIdAfter hnd'.2 hlM hxM ((List.mem_cons.mp ha).resolve_left (Ne.symm hac))

/-! ## The order of RGA states -/

theorem mem_order {s : List Rga.Node} {a : Id} :
    a ∈ order s ↔ ∃ n, n ∈ Rga.walk s.length s none ∧ n.id = a := by
  unfold order
  rw [List.mem_map]

/-- A valid state's order lists each atom once. -/
theorem order_nodup {s : List Rga.Node} (hs : Rga.Valid s) (hnd : s.Nodup) : (order s).Nodup := by
  unfold order
  apply List.pairwise_map.mpr
  exact (List.Pairwise.and_mem.mp (Rga.walk_nodup hs hnd _ _)).imp
    (fun ⟨ha, hb, hne⟩ e => hne (hs.id_inj _ _ (Rga.walk_subset ha) (Rga.walk_subset hb) e))

/-- `s'` is a later state of the story than `s`: both are states the RGA
reaches, and `s'` holds every insert of `s` — the same replica after more
inserts, or any replica that has received everything `s` holds. -/
structure Later (s s' : List Rga.Node) : Prop where
  valid : Rga.Valid s
  valid' : Rga.Valid s'
  nodup : s.Nodup
  nodup' : s'.Nodup
  sub : ∀ n, n ∈ s → n ∈ s'

/-- Replicas' states: a replica that received the inserts `ops'`, including
all of `ops`, is later than one that received `ops`. -/
theorem later_state {ops ops' : List Rga.Node} (hv : Rga.Valid ops) (hv' : Rga.Valid ops')
    (hsub : ∀ n, n ∈ ops → n ∈ ops') : Later (Rga.state ops) (Rga.state ops') where
  valid := Rga.valid_state.mpr hv
  valid' := Rga.valid_state.mpr hv'
  nodup := Rga.state_nodup ops
  nodup' := Rga.state_nodup ops'
  sub n h := Rga.state_mem.mpr (hsub n (Rga.state_mem.mp h))

theorem order_sublist {s s' : List Rga.Node} (h : Later s s') : (order s).Sublist (order s') := by
  unfold order
  have hlen : s.length ≤ s'.length := Rga.length_le_of_subset h.nodup h.sub
  obtain ⟨d, hd⟩ := Nat.exists_eq_add_of_le hlen
  rw [← Rga.walk_fuel h.valid d none, ← hd,
    ← Rga.walk_filter h.valid h.valid' h.nodup h.nodup' h.sub]
  exact List.filter_sublist.map _

/-- **Inserts never reorder**: a later state of the story keeps every atom of
an earlier one, in the same relative order — the hypothesis `Extends` of
`Marks.lean`, for every pair of states the RGA reaches. -/
theorem extends_order {s s' : List Rga.Node} (h : Later s s') : Extends (order s) (order s') :=
  extends_of_sublist (order_sublist h) (order_nodup h.valid' h.nodup')

/-- `x`, in the later state `s'` but not in `s`, was typed after `p` (`none`:
at the story start) by a replica that had seen every atom of `s` typed after
`p`: its Lamport time is greater than theirs. -/
structure Typed (s s' : List Rga.Node) (x : Rga.Node) (p : Option Id) : Prop where
  mem : x ∈ s'
  fresh : x ∉ s
  parent : x.parent = p
  newer : ∀ c, c ∈ s → c.parent = p → c.id.lamport < x.id.lamport

theorem Typed.valid_cons {s s' : List Rga.Node} {x : Rga.Node} {p : Option Id}
    (h : Later s s') (t : Typed s s' x p) (hp : ∀ q, p = some q → ∃ b, b ∈ s ∧ b.id = q) :
    Rga.Valid (x :: s) where
  id_inj a b ha hb := h.valid'.id_inj a b
    ((List.mem_cons.mp ha).elim (· ▸ t.mem) (h.sub a)) ((List.mem_cons.mp hb).elim (· ▸ t.mem) (h.sub b))
  parent_lt a q ha := h.valid'.parent_lt a q ((List.mem_cons.mp ha).elim (· ▸ t.mem) (h.sub a))
  parent_mem a q ha hq := by
    rcases List.mem_cons.mp ha with rfl | ha
    · obtain ⟨b, hb, hbq⟩ := hp q (t.parent.symm.trans hq)
      exact ⟨b, List.mem_cons_of_mem _ hb, hbq⟩
    · obtain ⟨b, hb, hbq⟩ := h.valid.parent_mem a q ha hq
      exact ⟨b, List.mem_cons_of_mem _ hb, hbq⟩

theorem Typed.later {s s' : List Rga.Node} {x : Rga.Node} {p : Option Id} (h : Later s s')
    (t : Typed s s' x p) (hp : ∀ q, p = some q → ∃ b, b ∈ s ∧ b.id = q) : Later (x :: s) s' where
  valid := t.valid_cons h hp
  valid' := h.valid'
  nodup := List.nodup_cons.mpr ⟨t.fresh, h.nodup⟩
  nodup' := h.nodup'
  sub n hn := (List.mem_cons.mp hn).elim (· ▸ t.mem) (h.sub n)

theorem Typed.sibGe {s s' : List Rga.Node} {x : Rga.Node} {p : Option Id} (t : Typed s s' x p) :
    ∀ c, c ∈ s → c.parent = x.parent → Rga.sibGe x c = true := by
  intro c hc hcp
  have := t.newer c hc (hcp.trans t.parent)
  unfold Rga.sibGe
  simp only [Bool.or_eq_true, decide_eq_true_eq]
  exact Or.inl this

theorem Typed.leaf {s s' : List Rga.Node} {x : Rga.Node} {p : Option Id} (h : Later s s')
    (t : Typed s s' x p) : Rga.children (x :: s) (some x.id) = [] := by
  apply List.eq_nil_iff_forall_not_mem.mpr
  intro c hc
  obtain ⟨hc, hcp⟩ := Rga.mem_children.mp hc
  rcases List.mem_cons.mp hc with rfl | hc
  · exact Nat.lt_irrefl _ (h.valid'.parent_lt c c.id t.mem hcp)
  · obtain ⟨b, hb, hbx⟩ := h.valid.parent_mem c x.id hc hcp
    exact t.fresh (h.valid'.id_inj b x (h.sub b hb) t.mem hbx ▸ hb)

theorem Typed.not_mem_order {s s' : List Rga.Node} {x : Rga.Node} {p : Option Id} (h : Later s s')
    (t : Typed s s' x p) : x.id ∉ order s := by
  intro hx
  obtain ⟨n, hn, hnx⟩ := mem_order.mp hx
  have hns := Rga.walk_subset hn
  exact t.fresh (h.valid'.id_inj n x (h.sub n hns) t.mem hnx ▸ hns)

theorem order_typed_after {s s' : List Rga.Node} {x : Rga.Node} {l : Id} (h : Later s s')
    (t : Typed s s' x (some l)) :
    order (x :: s) = insertIdAfter l x.id (order s) := by
  unfold order
  rw [List.length_cons, Rga.walk_insert t.sibGe (t.leaf h) s.length none
    (fun L hL hlen => by have := Rga.path_length_le h.valid hL; omega),
    t.parent, ite_eq_right (by simp), List.nil_append]
  unfold Rga.insertAfter insertIdAfter
  rw [List.map_flatMap, List.flatMap_map]
  apply Rga.flatMap_congr'
  intro n _
  rw [t.parent]
  by_cases hn : n.id = l <;> simp [hn]

theorem order_typed_at_start {s s' : List Rga.Node} {x : Rga.Node} (h : Later s s')
    (t : Typed s s' x none) : order (x :: s) = x.id :: order s := by
  unfold order
  rw [List.length_cons, Rga.walk_insert t.sibGe (t.leaf h) s.length none
    (fun L hL hlen => by have := Rga.path_length_le h.valid hL; omega),
    t.parent, ite_eq_left rfl]
  unfold Rga.insertAfter
  rw [t.parent]
  simp only [reduceCtorEq, ite_false, List.singleton_append, List.map_cons]
  congr 1
  induction Rga.walk s.length s none with
  | nil => rfl
  | cons n L ih => rw [List.flatMap_cons, List.singleton_append, List.map_cons, ih]; rfl

/-- **A newly typed atom lands right after its parent**: `x`, typed after
`l` by a replica that had seen every atom typed after `l` in `s`, is in the
gap right after `l` of every later state — whatever else was inserted
concurrently. The hypothesis `InGap` of `Marks.lean`. -/
theorem inGap_order {s s' : List Rga.Node} {x : Rga.Node} {l : Id} (h : Later s s')
    (t : Typed s s' x (some l)) (hl : l ∈ order s) : InGap (order s) (order s') l x.id := by
  obtain ⟨b, hbw, hbl⟩ := mem_order.mp hl
  have h₁ := t.later h (fun q hq => ⟨b, Rga.walk_subset hbw, hbl.trans (Option.some.inj hq)⟩)
  have ext₁ := extends_order h₁
  have hord := order_typed_after h t
  have hxm : x.id ∈ order (x :: s) := hord ▸ mem_insertIdAfter_self hl
  refine { ext := extends_order h, l_mem := hl, fresh := t.not_mem_order h,
           mem := ext₁.mem _ hxm, after := fun a ha => ?_ }
  rw [ext₁.lt_eq a x.id (hord ▸ mem_insertIdAfter ha) hxm, hord]
  exact lt_insertIdAfter (order_nodup h.valid h.nodup) hl (t.not_mem_order h) ha

/-- **An atom typed at the story start comes before every old atom** (the
hypothesis `AtStart` of `Marks.lean`), when its typist had seen every atom
typed at the start in `s`. -/
theorem atStart_order {s s' : List Rga.Node} {x : Rga.Node} (h : Later s s')
    (t : Typed s s' x none) : AtStart (order s) (order s') x.id := by
  have h₁ := t.later h (fun q hq => by cases hq)
  have ext₁ := extends_order h₁
  have hord := order_typed_at_start h t
  have hxm : x.id ∈ order (x :: s) := hord ▸ List.mem_cons_self
  refine { ext := extends_order h, fresh := t.not_mem_order h, mem := ext₁.mem _ hxm,
           first := fun a ha => ?_ }
  have hne : x.id ≠ a := fun e => t.not_mem_order h (e ▸ ha)
  rw [ext₁.lt_eq x.id a hxm (hord ▸ List.mem_cons_of_mem _ ha), hord, lt_cons_self hne]
  simp [ha]

/-! ## The mark theorems for RGA histories -/

/-- **Inserting text never reformats existing text**, for any later state of
the story. -/
theorem effective_preserved_rga {s s' : List Rga.Node} (h : Later s s') {ms : List Mark}
    (hms : ∀ m, m ∈ ms → Anchored (order s) m) {x : Id} (hx : x ∈ order s) (k : Nat) :
    effective (order s') ms x k = effective (order s) ms x k :=
  effective_preserved (extends_order h) hms hx k

/-- `effective_preserved_rga` for replicas: one that received more inserts
(any number, concurrent or not) formats the atoms the other has alike. -/
theorem format_preserved {ops ops' : List Rga.Node} {marks : List Mark} (hv : Rga.Valid ops)
    (hv' : Rga.Valid ops') (hsub : ∀ n, n ∈ ops → n ∈ ops')
    (hms : ∀ m, m ∈ marks → Anchored (order (Rga.state ops)) m) {x : Id}
    (hx : x ∈ order (Rga.state ops)) (k : Nat) :
    format ops' marks x k = format ops marks x k :=
  effective_preserved_rga (later_state hv hv' hsub)
    (fun m hm => hms m (markState_mem.mp hm)) hx k

/-- **A newly typed atom gets the formatting of the gap after its parent.** -/
theorem effective_in_gap_rga {s s' : List Rga.Node} {x : Rga.Node} {l : Id} (h : Later s s')
    (t : Typed s s' x (some l)) (hl : l ∈ order s) {ms : List Mark}
    (hms : ∀ m, m ∈ ms → Anchored (order s) m) (k : Nat) :
    effective (order s') ms x.id k =
      ((ms.filter (fun m => m.key == k && spansGapAfter (order s) m l)).map toWrite).foldl
        Lww.write none :=
  effective_in_gap (inGap_order h t hl) hms k

/-- **Typing inside a mark's range is covered**, whether the mark expands or
not. -/
theorem typed_inside_rga {s s' : List Rga.Node} {x : Rga.Node} {y : Id} (h : Later s s')
    (t : Typed s s' x (some y)) (hy : y ∈ order s) {id : Id} {key value : Nat} {first last : Id}
    {expand : Bool} (hf : first ∈ order s) (hl : last ∈ order s)
    (h₁ : le (order s) first y = true) (h₂ : lt (order s) y last = true) :
    covers (order s') (markOver (order s) id key value first last expand) x.id = true :=
  typed_inside (order_nodup h.valid h.nodup) hf hl (inGap_order h t hy) h₁ h₂

/-- **Typing right after a mark's last atom is covered exactly when the mark
expands.** -/
theorem typed_after_end_rga {s s' : List Rga.Node} {x : Rga.Node} {last : Id} (h : Later s s')
    (t : Typed s s' x (some last)) (hl : last ∈ order s) {id : Id} {key value : Nat} {first : Id}
    {expand : Bool} (hf : first ∈ order s) (hfl : le (order s) first last = true) :
    covers (order s') (markOver (order s) id key value first last expand) x.id = expand :=
  typed_after_end (order_nodup h.valid h.nodup) hf (inGap_order h t hl) hfl

/-- **Typing right before a mark's first atom is never covered.** -/
theorem typed_before_start_rga {s s' : List Rga.Node} {x : Rga.Node} {p : Id} (h : Later s s')
    (t : Typed s s' x (some p)) {id : Id} {key value : Nat} {first last : Id} {expand : Bool}
    (hl : last ∈ order s) (hp : succ (order s) p = some first) :
    covers (order s') (markOver (order s) id key value first last expand) x.id = false := by
  obtain ⟨i, hi, _⟩ := succ_spec hp
  exact typed_before_start (order_nodup h.valid h.nodup) hl hp (inGap_order h t (idx_mem hi))

theorem typed_at_story_start_rga {s s' : List Rga.Node} {x : Rga.Node} (h : Later s s')
    (t : Typed s s' x none) {id : Id} {key value : Nat} {first last : Id} {expand : Bool}
    (hf : first ∈ order s) (hl : last ∈ order s) :
    covers (order s') (markOver (order s) id key value first last expand) x.id = false :=
  typed_at_story_start hf hl (atStart_order h t)

/-! ## Why `Typed.newer` is needed -/

private def a₁ : Rga.Node := ⟨⟨1, 1⟩, none, 0⟩
private def a₂ : Rga.Node := ⟨⟨2, 1⟩, some ⟨1, 1⟩, 0⟩
/-- Typed after `a₂` by replica 1 before it made the mark. -/
private def c : Rga.Node := ⟨⟨4, 1⟩, some ⟨2, 1⟩, 0⟩
/-- Typed after `a₂` by replica 2, which had not seen `c`. -/
private def x : Rga.Node := ⟨⟨3, 2⟩, some ⟨2, 1⟩, 0⟩

/-- **An insert concurrent with a newer sibling does not land right after its
parent.** Replica 1 holds `a₁ a₂ c` (`c` typed after `a₂`) and makes bold over
`a₁ a₂`, expanding: it ends before `c`. Replica 2, which had seen only
`a₁ a₂`, types `x` after `a₂`. `x` is older than `c`, so it sorts after `c`:
the merged story is `a₁ a₂ c x`, `x` is not in the gap after `a₂`, and the
bold mark does not cover it although it was typed right after the bold text
on replica 2. Everything else of `Typed` holds; only `newer` fails. -/
theorem concurrent_typing_lands_after_unseen_sibling :
    let s := [c, a₂, a₁]
    let s' := [x, c, a₂, a₁]
    let bold := markOver (order s) ⟨10, 1⟩ 0 1 a₁.id a₂.id true
    Later s s' ∧ x ∈ s' ∧ x ∉ s ∧ x.parent = some a₂.id ∧
      ¬ (c.id.lamport < x.id.lamport) ∧
      ¬ InGap (order s) (order s') a₂.id x.id ∧
      covers (order s') bold x.id = false := by
  have hs : order [c, a₂, a₁] = [⟨1, 1⟩, ⟨2, 1⟩, ⟨4, 1⟩] := by
    simp [order, Rga.walk, Rga.children, a₁, a₂, c]
  have hs' : order [x, c, a₂, a₁] = [⟨1, 1⟩, ⟨2, 1⟩, ⟨4, 1⟩, ⟨3, 2⟩] := by
    simp [order, Rga.walk, Rga.children, List.mergeSort, Rga.sibGe, a₁, a₂, c, x]
  have hv : Rga.Valid [x, c, a₂, a₁] := by
    refine ⟨fun a b ha hb e => ?_, fun a q ha hq => ?_, fun a q ha hq => ?_⟩
    · simp only [List.mem_cons, List.not_mem_nil, or_false] at ha hb
      rcases ha with rfl | rfl | rfl | rfl <;> rcases hb with rfl | rfl | rfl | rfl <;>
        first | rfl | (simp [a₁, a₂, c, x] at e)
    · simp only [List.mem_cons, List.not_mem_nil, or_false] at ha
      rcases ha with rfl | rfl | rfl | rfl <;> simp [a₁, a₂, c, x] at hq ⊢ <;> subst hq <;> decide
    · simp only [List.mem_cons, List.not_mem_nil, or_false] at ha
      rcases ha with rfl | rfl | rfl | rfl <;> simp [a₁, a₂, c, x] at hq <;> subst hq
      · exact ⟨a₂, by simp, rfl⟩
      · exact ⟨a₂, by simp, rfl⟩
      · exact ⟨a₁, by simp, rfl⟩
  have hsub : ∀ n, n ∈ [c, a₂, a₁] → n ∈ [x, c, a₂, a₁] := fun n hn => List.mem_cons_of_mem _ hn
  have hv₀ : Rga.Valid [c, a₂, a₁] := by
    refine ⟨fun a b ha hb => hv.id_inj a b (hsub a ha) (hsub b hb),
      fun a q ha => hv.parent_lt a q (hsub a ha), fun a q ha hq => ?_⟩
    simp only [List.mem_cons, List.not_mem_nil, or_false] at ha
    rcases ha with rfl | rfl | rfl <;> simp [a₁, a₂, c] at hq <;> subst hq
    · exact ⟨a₂, by simp, rfl⟩
    · exact ⟨a₁, by simp, rfl⟩
  refine ⟨⟨hv₀, hv, by decide, by decide, hsub⟩, by simp, by decide, rfl, by decide, ?_, ?_⟩
  · intro g
    have := g.after c.id (by rw [hs]; simp [c])
    rw [hs, hs'] at this
    revert this
    decide
  · simp only [hs, hs']
    decide

end JadeCollab.Marks
