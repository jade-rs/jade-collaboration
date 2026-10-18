import JadeCollab.Rga

/-!
# The shape of the RGA walk

Facts about `Rga.walk` on the insert sets the RGA can actually reach, which
`MarksRga.lean` turns into the hypotheses `Marks.lean` takes (`Extends`,
`InGap`, `AtStart`).

`Rga::insert` (`crates/jade-collab-core/src/rga.rs`) refuses an atom whose id
is taken, whose parent it does not have (`MissingParent`) or whose parent is
not older (`NotNewerThanParent`): Lamport clocks make every insert newer than
the atom it was typed after. `Valid` is exactly that, as a property of the
insert *set*, so every replica's state satisfies it.

* `walk_nodup`: the walk visits each atom at most once.
* `walk_fuel`: fuel `s.length` reaches every depth (a chain from the story
  start has increasing Lamport times, so at most `s.length` atoms).
* `walk_filter`: the walk of a larger valid set, restricted to the atoms of a
  smaller one, is the smaller set's walk — inserts never reorder existing
  atoms.
* `walk_insert`: an atom newer than its siblings and with no children is
  visited right after its parent (`insertAfter`).
-/

namespace JadeCollab.Rga

/-- The insert sets `Rga::insert` reaches: ids are unique, and every atom's
parent is present and has a smaller Lamport time. Stated on membership only,
so it is a property of the set (`valid_state`). -/
structure Valid (s : List Node) : Prop where
  id_inj : ∀ a b, a ∈ s → b ∈ s → a.id = b.id → a = b
  parent_lt : ∀ a q, a ∈ s → a.parent = some q → q.lamport < a.id.lamport
  parent_mem : ∀ a q, a ∈ s → a.parent = some q → ∃ b, b ∈ s ∧ b.id = q

theorem Valid.congr {s₁ s₂ : List Node} (h : ∀ n, n ∈ s₁ ↔ n ∈ s₂) (v : Valid s₁) :
    Valid s₂ where
  id_inj a b ha hb := v.id_inj a b ((h a).mpr ha) ((h b).mpr hb)
  parent_lt a q ha := v.parent_lt a q ((h a).mpr ha)
  parent_mem a q ha hp := by
    obtain ⟨b, hb, hq⟩ := v.parent_mem a q ((h a).mpr ha) hp
    exact ⟨b, (h b).mp hb, hq⟩

/-- A replica's state is valid exactly when the inserts it received are. -/
theorem valid_state {ops : List Node} : Valid (state ops) ↔ Valid ops :=
  ⟨Valid.congr (fun _ => state_mem), Valid.congr (fun _ => state_mem.symm)⟩

theorem mem_children {s : List Node} {p : Option Id} {c : Node} :
    c ∈ children s p ↔ c ∈ s ∧ c.parent = p := by
  unfold children
  rw [(List.mergeSort_perm _ _).mem_iff, List.mem_filter, beq_iff_eq]

theorem children_nodup {s : List Node} (hnd : s.Nodup) (p : Option Id) :
    (children s p).Nodup :=
  (List.mergeSort_perm _ _).nodup_iff.mpr (hnd.sublist List.filter_sublist)

theorem flatMap_congr' {α β : Type} {l : List α} {f g : α → List β}
    (h : ∀ a, a ∈ l → f a = g a) : l.flatMap f = l.flatMap g := by
  induction l with
  | nil => rfl
  | cons a l ih =>
    rw [List.flatMap_cons, List.flatMap_cons, h a List.mem_cons_self,
      ih (fun b hb => h b (List.mem_cons_of_mem _ hb))]

/-- Members a `flatMap` sends to `[]` can be filtered out first. -/
theorem flatMap_filter_of_nil {α β : Type} {l : List α} {f : α → List β} (P : α → Bool)
    (h : ∀ a, a ∈ l → P a = false → f a = []) : l.flatMap f = (l.filter P).flatMap f := by
  induction l with
  | nil => rfl
  | cons a l ih =>
    have ih := ih (fun b hb => h b (List.mem_cons_of_mem _ hb))
    rw [List.flatMap_cons, List.filter_cons]
    cases hp : P a
    · rw [h a List.mem_cons_self hp, ih]; rfl
    · rw [ite_eq_left rfl, List.flatMap_cons, ih]

/-! ## Fuel -/

/-- A chain of atoms going down from `p`: each one a child of the previous. -/
inductive Path (s : List Node) : Option Id → List Node → Prop
  | nil (p : Option Id) : Path s p []
  | cons {p : Option Id} {c : Node} {L : List Node} :
      c ∈ s → c.parent = p → Path s (some c.id) L → Path s p (c :: L)

/-- One more unit of fuel changes the walk only if some chain is that long. -/
theorem walk_succ_eq (s : List Node) :
    ∀ k p, (∀ L, Path s p L → L.length ≠ k + 1) → walk k s p = walk (k + 1) s p := by
  intro k
  induction k with
  | zero =>
    intro p h
    have hc : children s p = [] := by
      cases hc : children s p with
      | nil => rfl
      | cons c cs =>
        have hm : c ∈ children s p := hc ▸ List.mem_cons_self
        obtain ⟨hcs, hcp⟩ := mem_children.mp hm
        exact absurd rfl (h [c] (Path.cons hcs hcp (Path.nil _)))
    simp [walk, hc]
  | succ k ih =>
    intro p h
    show (children s p).flatMap (fun c => c :: walk k s (some c.id)) =
      (children s p).flatMap (fun c => c :: walk (k + 1) s (some c.id))
    apply flatMap_congr'
    intro c hc
    obtain ⟨hcs, hcp⟩ := mem_children.mp hc
    rw [ih (some c.id) (fun L hL hlen => h (c :: L) (Path.cons hcs hcp hL) (by simp [hlen]))]

theorem Path.subset {s : List Node} {p : Option Id} {L : List Node} (h : Path s p L) :
    ∀ e, e ∈ L → e ∈ s := by
  induction h with
  | nil => intro e he; cases he
  | cons hc _ _ ih =>
    intro e he
    rcases List.mem_cons.mp he with rfl | he
    · exact hc
    · exact ih e he

theorem Path.lamport_lt {s : List Node} (hs : Valid s) {q : Id} {L : List Node}
    (h : Path s (some q) L) : ∀ e, e ∈ L → q.lamport < e.id.lamport := by
  generalize hp : some q = p at h
  induction h generalizing q with
  | nil => intro e he; cases he
  | cons hc hcp _ ih =>
    intro e he
    have h₁ := hs.parent_lt _ q hc (hcp.trans hp.symm)
    rcases List.mem_cons.mp he with rfl | he
    · exact h₁
    · exact Nat.lt_trans h₁ (ih rfl e he)

theorem Path.pairwise {s : List Node} (hs : Valid s) {p : Option Id} {L : List Node}
    (h : Path s p L) : L.Pairwise (fun a b => a.id.lamport < b.id.lamport) := by
  induction h with
  | nil => exact List.Pairwise.nil
  | cons _ _ hL ih => exact List.pairwise_cons.mpr ⟨hL.lamport_lt hs, ih⟩

/-- A duplicate-free list inside `s` is no longer than `s`. -/
theorem length_le_of_subset {α : Type} [DecidableEq α] :
    ∀ {l s : List α}, l.Nodup → (∀ a, a ∈ l → a ∈ s) → l.length ≤ s.length
  | [], _, _, _ => Nat.zero_le _
  | a :: l, s, hnd, hsub => by
    have ha : a ∈ s := hsub a List.mem_cons_self
    have hnd' := List.nodup_cons.mp hnd
    have ih := length_le_of_subset (s := s.erase a) hnd'.2 (fun b hb =>
      (List.mem_erase_of_ne (fun (e : b = a) => hnd'.1 (e ▸ hb))).mpr (hsub b (List.mem_cons_of_mem _ hb)))
    rw [List.length_erase_of_mem ha] at ih
    have := List.length_pos_of_mem ha
    simp only [List.length_cons]
    omega

/-- No chain from the story start is longer than the insert set. -/
theorem path_length_le {s : List Node} (hs : Valid s) {p : Option Id} {L : List Node}
    (h : Path s p L) : L.length ≤ s.length :=
  length_le_of_subset ((h.pairwise hs).imp (fun hab e => by subst e; exact Nat.lt_irrefl _ hab))
    h.subset

theorem walk_stable {s : List Node} (hs : Valid s) {k : Nat} (hk : s.length ≤ k) (p : Option Id) :
    walk k s p = walk (k + 1) s p :=
  walk_succ_eq s k p (fun L hL hlen => by have := path_length_le hs hL; omega)

/-- Fuel `s.length` is enough: more fuel does not change the walk. -/
theorem walk_fuel {s : List Node} (hs : Valid s) (d : Nat) (p : Option Id) :
    walk (s.length + d) s p = walk s.length s p := by
  induction d with
  | zero => rfl
  | succ d ih => rw [← Nat.add_assoc, ← walk_stable hs (Nat.le_add_right _ _), ih]

/-! ## Each atom once -/

/-- `d` is a descendant of `p` in the parent tree of `s`. -/
inductive Below (s : List Node) (p : Option Id) : Node → Prop
  | child {d : Node} : d ∈ s → d.parent = p → Below s p d
  | step {e d : Node} : Below s p e → d ∈ s → d.parent = some e.id → Below s p d

theorem Below.mem {s : List Node} {p : Option Id} {d : Node} (h : Below s p d) : d ∈ s := by
  cases h with
  | child hd _ => exact hd
  | step _ hd _ => exact hd

theorem Below.trans {s : List Node} {p : Option Id} {c d : Node} (h : Below s (some c.id) d)
    (hc : Below s p c) : Below s p d := by
  generalize hq : some c.id = q at h
  induction h with
  | child hd hdp => exact Below.step hc hd (hdp.trans hq.symm)
  | step _ hd hdp ih => exact Below.step ih hd hdp

theorem Below.parent_ge {s : List Node} (hs : Valid s) {q : Id} {d : Node}
    (h : Below s (some q) d) : ∃ r, d.parent = some r ∧ q.lamport ≤ r.lamport := by
  generalize hp : some q = p at h
  induction h with
  | child _ hdp => exact ⟨q, hdp.trans hp.symm, Nat.le_refl _⟩
  | step he _ hdp ih =>
    obtain ⟨r, her, hr⟩ := ih
    exact ⟨_, hdp, Nat.le_of_lt (Nat.lt_of_le_of_lt hr (hs.parent_lt _ r he.mem her))⟩

theorem Below.lamport_lt {s : List Node} (hs : Valid s) {q : Id} {d : Node}
    (h : Below s (some q) d) : q.lamport < d.id.lamport := by
  obtain ⟨r, hr, hle⟩ := h.parent_ge hs
  exact Nat.lt_of_le_of_lt hle (hs.parent_lt d r h.mem hr)

/-- A sibling of `c` is never below `c`. -/
theorem Below.not_sibling {s : List Node} (hs : Valid s) {c d : Node} (hc : c ∈ s)
    (h : Below s (some c.id) d) (hp : c.parent = d.parent) : False := by
  obtain ⟨r, hr, hle⟩ := h.parent_ge hs
  have := hs.parent_lt c r hc (hp.trans hr)
  omega

/-- Two ancestors of the same atom are one above the other. -/
theorem Below.unique {s : List Node} (hs : Valid s) {p₁ p₂ : Option Id} {d : Node}
    (h₁ : Below s p₁ d) (h₂ : Below s p₂ d) :
    p₁ = p₂ ∨ (∃ e, some e.id = p₁ ∧ Below s p₂ e) ∨ (∃ e, some e.id = p₂ ∧ Below s p₁ e) := by
  induction h₁ with
  | child hd hdp =>
    cases h₂ with
    | child _ hdp₂ => exact Or.inl (hdp.symm.trans hdp₂)
    | step he _ hdp₂ => exact Or.inr (Or.inl ⟨_, hdp₂.symm.trans hdp, he⟩)
  | step he hd hdp ih =>
    cases h₂ with
    | child _ hdp₂ => exact Or.inr (Or.inr ⟨_, hdp.symm.trans hdp₂, he⟩)
    | step he₂ _ hdp₂ =>
      have hid := Option.some.inj (hdp.symm.trans hdp₂)
      have := hs.id_inj _ _ he.mem he₂.mem hid
      subst this
      exact ih he₂

theorem mem_walk {s : List Node} : ∀ {k : Nat} {p : Option Id} {d : Node},
    d ∈ walk k s p → Below s p d
  | 0, _, _, h => by cases h
  | k + 1, p, d, h => by
    obtain ⟨c, hc, hd⟩ := List.mem_flatMap.mp h
    obtain ⟨hcs, hcp⟩ := mem_children.mp hc
    have hbc : Below s p c := Below.child hcs hcp
    rcases List.mem_cons.mp hd with rfl | hd
    · exact hbc
    · exact (mem_walk hd).trans hbc

theorem walk_subset {s : List Node} {k : Nat} {p : Option Id} {d : Node}
    (h : d ∈ walk k s p) : d ∈ s :=
  (mem_walk h).mem

/-- **The walk visits every atom at most once.** -/
theorem walk_nodup {s : List Node} (hs : Valid s) (hnd : s.Nodup) :
    ∀ k p, (walk k s p).Nodup := by
  intro k
  induction k with
  | zero => intro p; exact List.nodup_nil
  | succ k ih =>
    intro p
    show List.Pairwise (· ≠ ·) ((children s p).flatMap (fun c => c :: walk k s (some c.id)))
    refine List.pairwise_flatMap.mpr ⟨fun c hc => ?_, ?_⟩
    · obtain ⟨hcs, _⟩ := mem_children.mp hc
      refine List.nodup_cons.mpr ⟨fun hin => ?_, ih _⟩
      have := (mem_walk hin).lamport_lt hs
      exact Nat.lt_irrefl _ this
    · refine (List.Pairwise.and_mem.mp (children_nodup hnd p)).imp ?_
      rintro c₁ c₂ ⟨h₁, h₂, hne⟩ x hx y hy rfl
      obtain ⟨hc₁, hp₁⟩ := mem_children.mp h₁
      obtain ⟨hc₂, hp₂⟩ := mem_children.mp h₂
      have hsib : c₁.parent = c₂.parent := hp₁.trans hp₂.symm
      rcases List.mem_cons.mp hx with rfl | hx <;> rcases List.mem_cons.mp hy with hxy | hy
      · exact hne hxy
      · exact (mem_walk hy).not_sibling hs hc₂ hsib.symm
      · subst hxy; exact (mem_walk hx).not_sibling hs hc₁ hsib
      · rcases (mem_walk hx).unique hs (mem_walk hy) with h | ⟨e, he, hb⟩ | ⟨e, he, hb⟩
        · exact hne (hs.id_inj _ _ hc₁ hc₂ (Option.some.inj h))
        · have := hs.id_inj _ _ hb.mem hc₁ (Option.some.inj he)
          subst this
          exact hb.not_sibling hs hc₂ hsib.symm
        · have := hs.id_inj _ _ hb.mem hc₂ (Option.some.inj he)
          subst this
          exact hb.not_sibling hs hc₁ hsib

/-! ## Inserts never reorder -/

/-- The children of `p` in a larger set, restricted to a smaller one. -/
theorem children_filter {s s' : List Node} (hnd : s.Nodup) (hnd' : s'.Nodup)
    (hsub : ∀ n, n ∈ s → n ∈ s') (p : Option Id) :
    (children s' p).filter (fun n => decide (n ∈ s)) = children s p := by
  apply List.Perm.eq_of_pairwise (le := fun a b => sibGe a b = true)
  · intro a b ha hb hab hba
    have ha' := (mem_children.mp (List.mem_filter.mp ha).1).2
    have hb' := (mem_children.mp hb).2
    exact sibGe_antisymm a b (ha'.trans hb'.symm) hab hba
  · exact (List.pairwise_mergeSort sibGe_trans sibGe_total _).filter _
  · exact List.pairwise_mergeSort sibGe_trans sibGe_total _
  · refine (((List.mergeSort_perm _ _).filter _).trans ?_).trans (List.mergeSort_perm _ _).symm
    apply (List.perm_ext_iff_of_nodup ((hnd'.sublist List.filter_sublist).sublist
      List.filter_sublist) (hnd.sublist List.filter_sublist)).mpr
    intro n
    simp only [List.mem_filter, beq_iff_eq, decide_eq_true_eq]
    constructor
    · rintro ⟨⟨_, h⟩, hs⟩; exact ⟨hs, h⟩
    · rintro ⟨hs, h⟩; exact ⟨⟨hsub n hs, h⟩, hs⟩

/-- An atom of the smaller set below `q` in the larger one has `q` in the
smaller set too: the smaller set is closed under parents. -/
theorem Below.anchor_mem {s s' : List Node} (hs : Valid s) (hs' : Valid s')
    (hsub : ∀ n, n ∈ s → n ∈ s') {q : Id} {d : Node} (h : Below s' (some q) d) (hd : d ∈ s) :
    ∃ e, e ∈ s ∧ e.id = q := by
  generalize hp : some q = p at h
  induction h with
  | child _ hdp => exact hs.parent_mem _ q hd (hdp.trans hp.symm)
  | step he _ hdp ih =>
    obtain ⟨b, hb, hbe⟩ := hs.parent_mem _ _ hd hdp
    have := hs'.id_inj _ _ (hsub b hb) he.mem hbe
    subst this
    exact ih hb

/-- **Restricting the walk of a larger valid set to the atoms of a smaller
valid set gives the smaller set's walk**: adding inserts never moves the
atoms already there relative to each other. -/
theorem walk_filter {s s' : List Node} (hs : Valid s) (hs' : Valid s') (hnd : s.Nodup)
    (hnd' : s'.Nodup) (hsub : ∀ n, n ∈ s → n ∈ s') :
    ∀ k p, (walk k s' p).filter (fun n => decide (n ∈ s)) = walk k s p := by
  intro k
  induction k with
  | zero => intro p; rfl
  | succ k ih =>
    intro p
    show ((children s' p).flatMap (fun c => c :: walk k s' (some c.id))).filter _ =
      (children s p).flatMap (fun c => c :: walk k s (some c.id))
    rw [List.filter_flatMap, flatMap_filter_of_nil (fun n => decide (n ∈ s)),
      children_filter hnd hnd' hsub]
    · apply flatMap_congr'
      intro c hc
      have hcs := (mem_children.mp hc).1
      rw [List.filter_cons, ite_eq_left (decide_eq_true hcs), ih]
    · intro c hc' hc
      have hcs : c ∉ s := of_decide_eq_false hc
      have hcs' := (mem_children.mp hc').1
      rw [List.filter_cons, ite_eq_right (by simp [hcs])]
      apply List.filter_eq_nil_iff.mpr
      intro d hd hds
      obtain ⟨e, he, hec⟩ := (mem_walk hd).anchor_mem hs hs' hsub (of_decide_eq_true hds)
      exact hcs (hs'.id_inj _ _ (hsub e he) hcs' hec ▸ he)

/-! ## Where a new atom lands -/

/-- Visit `x` right after its parent. -/
def insertAfter (x : Node) (L : List Node) : List Node :=
  L.flatMap (fun n => if some n.id = x.parent then [n, x] else [n])

theorem insertAfter_cons (x c : Node) (L : List Node) :
    insertAfter x (c :: L) = (if some c.id = x.parent then [c, x] else [c]) ++ insertAfter x L :=
  List.flatMap_cons

/-- An atom that sorts before every sibling of its parent comes first. -/
theorem children_cons {s : List Node} {x : Node}
    (hsib : ∀ c, c ∈ s → c.parent = x.parent → sibGe x c = true) (p : Option Id) :
    children (x :: s) p = (if p = x.parent then [x] else []) ++ children s p := by
  by_cases hp : p = x.parent
  · rw [ite_eq_left hp]
    apply List.Perm.eq_of_pairwise (le := fun a b => sibGe a b = true)
    · intro a b ha hb hab hba
      have ha' := (mem_children.mp ha).2
      have hb' : b.parent = p := by
        rcases List.mem_cons.mp hb with rfl | hb
        · exact hp.symm
        · exact (mem_children.mp hb).2
      exact sibGe_antisymm a b (ha'.trans hb'.symm) hab hba
    · exact List.pairwise_mergeSort sibGe_trans sibGe_total _
    · refine List.pairwise_cons.mpr ⟨fun c hc => ?_, List.pairwise_mergeSort sibGe_trans sibGe_total _⟩
      obtain ⟨hcs, hcp⟩ := mem_children.mp hc
      exact hsib c hcs (hcp.trans hp)
    · unfold children
      rw [List.filter_cons, ite_eq_left (by simp [hp])]
      exact (List.mergeSort_perm _ _).trans ((List.mergeSort_perm _ _).symm.cons x)
  · rw [ite_eq_right hp, List.nil_append]
    unfold children
    rw [List.filter_cons, ite_eq_right (by simp only [beq_iff_eq]; exact fun h => hp h.symm)]

/-- **A new atom that sorts before its siblings and has no children is visited
right after its parent** (at the start when its parent is the story start). -/
theorem walk_insert {s : List Node} {x : Node}
    (hsib : ∀ c, c ∈ s → c.parent = x.parent → sibGe x c = true)
    (hleaf : children (x :: s) (some x.id) = []) :
    ∀ k p, (∀ L, Path s p L → L.length ≠ k + 1) →
      walk (k + 1) (x :: s) p = (if p = x.parent then [x] else []) ++ insertAfter x (walk k s p) := by
  intro k
  induction k with
  | zero =>
    intro p h
    have hc : children s p = [] := by
      cases hc : children s p with
      | nil => rfl
      | cons c cs =>
        have hm : c ∈ children s p := hc ▸ List.mem_cons_self
        obtain ⟨hcs, hcp⟩ := mem_children.mp hm
        exact absurd rfl (h [c] (Path.cons hcs hcp (Path.nil _)))
    show (children (x :: s) p).flatMap (fun c => c :: walk 0 (x :: s) (some c.id)) = _
    rw [children_cons hsib, hc]
    by_cases hp : p = x.parent <;> simp [hp, walk, insertAfter]
  | succ k ih =>
    intro p h
    show (children (x :: s) p).flatMap (fun c => c :: walk (k + 1) (x :: s) (some c.id)) =
      _ ++ insertAfter x ((children s p).flatMap (fun c => c :: walk k s (some c.id)))
    rw [children_cons hsib, List.flatMap_append, insertAfter, List.flatMap_assoc]
    congr 1
    · by_cases hp : p = x.parent
      · simp only [hp, ite_true, List.flatMap_cons, List.flatMap_nil, List.append_nil]
        show [x] ++ (children (x :: s) (some x.id)).flatMap _ = [x]
        rw [hleaf]; rfl
      · simp [hp]
    · apply flatMap_congr'
      intro c hc
      obtain ⟨hcs, hcp⟩ := mem_children.mp hc
      rw [ih (some c.id) (fun L hL hlen => h (c :: L) (Path.cons hcs hcp hL) (by simp [hlen]))]
      show _ = insertAfter x (c :: walk k s (some c.id))
      rw [insertAfter_cons]
      by_cases hq : some c.id = x.parent <;> simp [hq]

end JadeCollab.Rga
