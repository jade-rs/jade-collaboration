import JadeCollab.Sequencer

/-!
# Tables stay anchored to visible paragraph marks

design.md §4 (Text, Tables): a story is a sequence of atoms — characters and
paragraph marks — and a table is anchored *after* a paragraph mark (or at the
start of the story). Splitting a paragraph inserts a mark and is convergent;
joining two paragraphs deletes a mark and is sequenced, because a table may be
anchored to it. This module proves that this split keeps every live table
anchored to a visible mark, and that the convergent operations never change
whether a sequenced one is accepted — which is why a replica may apply them
straight to its optimistic view (`Reconcile.fast_path_sound`) while the server
validates joins and table insertions. Backs `join` / `insert_table` in
`crates/jade-docx-collab/src/model.rs`.

The model is deliberately small: one story, atoms with tombstones (`visible`),
and the RGA order abstracted to "insert after an existing atom".
-/

namespace JadeCollab.Anchor

inductive Kind
  | char
  | mark
deriving DecidableEq, Repr

structure Atom where
  id : Nat
  kind : Kind
  visible : Bool
deriving DecidableEq, Repr

/-- A table placed after mark `m` (`anchor = some m`) or at the story start. -/
structure Table where
  id : Nat
  anchor : Option Nat
  live : Bool
deriving DecidableEq, Repr

structure Story where
  atoms : List Atom
  tables : List Table
deriving DecidableEq, Repr

inductive Op
  /-- Convergent: type a character after an atom (`none`: at the start). -/
  | insertChar (id : Nat) (after : Option Nat)
  /-- Convergent: split a paragraph by inserting a mark. -/
  | insertMark (id : Nat) (after : Option Nat)
  /-- Convergent: delete a character. A mark cannot be its target. -/
  | deleteChar (id : Nat)
  /-- Sequenced: join two paragraphs by deleting the mark between them. -/
  | join (mark : Nat)
  /-- Sequenced: insert a table after a mark (`none`: at the story start). -/
  | insertTable (id : Nat) (anchor : Option Nat)
  /-- Sequenced: delete a table (tombstone). -/
  | deleteTable (id : Nat)
deriving DecidableEq, Repr

def Op.convergent : Op → Bool
  | .insertChar .. | .insertMark .. | .deleteChar .. => true
  | _ => false

/-- The mark an operation creates. -/
def Op.newMark : Op → Option Nat
  | .insertMark id _ => some id
  | _ => none

/-- The mark a sequenced operation names. -/
def Op.markRef : Op → Option Nat
  | .join m => some m
  | .insertTable _ (some m) => some m
  | _ => none

def visibleMark (atoms : List Atom) (m : Nat) : Bool :=
  atoms.any fun a => a.id == m && a.kind == .mark && a.visible

/-- Insert after the atom `after` (it must exist), with a fresh identifier. -/
def insertAtom (atoms : List Atom) (after : Option Nat) (a : Atom) : Option (List Atom) :=
  if atoms.any (·.id == a.id) then none
  else
    match after with
    | none => some (a :: atoms)
    | some p =>
      match atoms.findIdx? (·.id == p) with
      | none => none
      | some i => some (atoms.take (i + 1) ++ a :: atoms.drop (i + 1))

def hideChar (id : Nat) (a : Atom) : Atom :=
  if a.id = id ∧ a.kind = .char then { a with visible := false } else a

def hideMark (id : Nat) (a : Atom) : Atom :=
  if a.id = id ∧ a.kind = .mark then { a with visible := false } else a

/-- A live table is anchored after mark `m`. -/
def blocked (tables : List Table) (m : Nat) : Bool :=
  tables.any fun t => t.live && t.anchor == some m

def apply (s : Story) : Op → Option Story
  | .insertChar id after =>
    (insertAtom s.atoms after ⟨id, .char, true⟩).map fun as => { s with atoms := as }
  | .insertMark id after =>
    (insertAtom s.atoms after ⟨id, .mark, true⟩).map fun as => { s with atoms := as }
  | .deleteChar id =>
    if s.atoms.any (fun a => a.id == id && a.kind == .char) then
      some { s with atoms := s.atoms.map (hideChar id) }
    else none
  | .join m =>
    if visibleMark s.atoms m && !blocked s.tables m then
      some { s with atoms := s.atoms.map (hideMark m) }
    else none
  | .insertTable id anchor =>
    if !(s.tables.any (·.id == id)) && (match anchor with
        | none => true
        | some m => visibleMark s.atoms m) then
      some { s with tables := s.tables ++ [⟨id, anchor, true⟩] }
    else none
  | .deleteTable id =>
    if s.tables.any (fun t => t.id == id && t.live) then
      some { s with tables := s.tables.map fun t => if t.id = id then { t with live := false } else t }
    else none

/-- **The anchoring invariant**: every live table hangs off a visible mark. -/
def Anchored (s : Story) : Prop :=
  ∀ t ∈ s.tables, t.live = true → ∀ m, t.anchor = some m → visibleMark s.atoms m = true

/-! ## Visibility of marks under each operation -/

theorem any_insert (l : List Atom) (k : Nat) (a : Atom) (p : Atom → Bool) :
    (l.take k ++ a :: l.drop k).any p = (l.any p || p a) := by
  conv => rhs; rw [← List.take_append_drop k l]
  simp only [List.any_append, List.any_cons]
  cases p a <;> simp

theorem isSome_ite {α : Type} (p : Prop) [Decidable p] (a b : α) :
    (if p then some a else none).isSome = (if p then some b else none).isSome := by
  split <;> rfl

theorem visibleMark_insertAtom {atoms atoms' : List Atom} {after : Option Nat} {a : Atom}
    (h : insertAtom atoms after a = some atoms') (m : Nat) :
    visibleMark atoms' m = (visibleMark atoms m || (a.id == m && a.kind == .mark && a.visible)) := by
  unfold insertAtom at h
  split at h
  · simp at h
  · split at h
    · cases h; simp [visibleMark, Bool.or_comm]
    · split at h
      · simp at h
      · cases h
        simp only [visibleMark]
        exact any_insert _ _ _ _

theorem visibleMark_hideChar (atoms : List Atom) (id m : Nat) :
    visibleMark (atoms.map (hideChar id)) m = visibleMark atoms m := by
  simp only [visibleMark, List.any_map]
  congr 1; funext a
  unfold hideChar
  by_cases h : a.id = id ∧ a.kind = .char <;> simp [h]

theorem visibleMark_hideMark (atoms : List Atom) {id m : Nat} (hne : id ≠ m) :
    visibleMark (atoms.map (hideMark id)) m = visibleMark atoms m := by
  simp only [visibleMark, List.any_map]
  congr 1; funext a
  unfold hideMark
  by_cases h : a.id = id ∧ a.kind = .mark
  · simp [h, hne]
  · simp [h]

/-! ## Every accepted operation keeps tables anchored -/

theorem apply_preserves_anchored {s s' : Story} {op : Op} (hs : Anchored s)
    (h : apply s op = some s') : Anchored s' := by
  cases op with
  | insertChar id after =>
    simp only [apply, Option.map_eq_some_iff] at h
    obtain ⟨as, has, rfl⟩ := h
    intro t ht hl m hm
    simp [visibleMark_insertAtom has, hs t ht hl m hm]
  | insertMark id after =>
    simp only [apply, Option.map_eq_some_iff] at h
    obtain ⟨as, has, rfl⟩ := h
    intro t ht hl m hm
    simp [visibleMark_insertAtom has, hs t ht hl m hm]
  | deleteChar id =>
    simp only [apply] at h
    split at h
    · cases h
      intro t ht hl m hm
      simp only [visibleMark_hideChar]; exact hs t ht hl m hm
    · simp at h
  | join j =>
    simp only [apply] at h
    split at h
    · rename_i hc
      cases h
      simp only [Bool.and_eq_true, Bool.not_eq_true'] at hc
      intro t ht hl m hm
      have hne : j ≠ m := by
        rintro rfl
        have : blocked s.tables j = true := by
          simp only [blocked, List.any_eq_true, Bool.and_eq_true, beq_iff_eq]
          exact ⟨t, ht, hl, hm⟩
        rw [hc.2] at this; cases this
      simp only [visibleMark_hideMark _ hne]; exact hs t ht hl m hm
    · simp at h
  | insertTable id anchor =>
    cases anchor with
    | none =>
      simp [apply] at h
      obtain ⟨-, rfl⟩ := h
      intro t ht hl m hm
      rcases List.mem_append.mp ht with ht | ht
      · exact hs t ht hl m hm
      · simp at ht; subst ht; simp at hm
    | some a =>
      simp [apply] at h
      obtain ⟨⟨-, hva⟩, rfl⟩ := h
      intro t ht hl m hm
      rcases List.mem_append.mp ht with ht | ht
      · exact hs t ht hl m hm
      · simp at ht; subst ht
        simp only [Option.some.injEq] at hm; subst hm
        exact hva
  | deleteTable id =>
    simp only [apply] at h
    split at h
    · cases h
      intro t ht hl m hm
      simp only [List.mem_map] at ht
      obtain ⟨t0, ht0, rfl⟩ := ht
      by_cases hid : t0.id = id
      · simp [hid] at hl
      · simp only [hid, ite_false] at hl hm
        exact hs t0 ht0 hl m hm
    · simp at h

/-- Any log, folded by the sequencer, keeps every live table anchored to a
visible mark. -/
theorem anchored_sequenced {s : Story} (hs : Anchored s) (ops : List Op) :
    Anchored (ops.foldl (Sequencer.step apply) s) :=
  Sequencer.sequenced_preserves apply Anchored (fun _ _ _ hs h => apply_preserves_anchored hs h)
    ops s hs

/-! ## Convergent operations never decide a sequenced one -/

/-- A convergent operation leaves the tables alone and changes the visibility
of no mark other than the one it creates. -/
theorem convergent_effect {s s' : Story} {c : Op} (hc : c.convergent = true)
    (h : apply s c = some s') :
    s'.tables = s.tables ∧ ∀ m, c.newMark ≠ some m → visibleMark s'.atoms m = visibleMark s.atoms m := by
  cases c with
  | insertChar id after =>
    simp only [apply, Option.map_eq_some_iff] at h
    obtain ⟨as, has, rfl⟩ := h
    refine ⟨rfl, fun m _ => ?_⟩
    simp [visibleMark_insertAtom has]
  | insertMark id after =>
    simp only [apply, Option.map_eq_some_iff] at h
    obtain ⟨as, has, rfl⟩ := h
    refine ⟨rfl, fun m hm => ?_⟩
    simp only [Op.newMark, ne_eq, Option.some.injEq] at hm
    simp [visibleMark_insertAtom has, hm]
  | deleteChar id =>
    simp only [apply] at h
    split at h
    · cases h; exact ⟨rfl, fun m _ => visibleMark_hideChar _ _ _⟩
    · simp at h
  | join _ | insertTable _ _ | deleteTable _ => simp [Op.convergent] at hc

/-- **Convergent operations never affect whether a join or a table insertion
(or deletion) is accepted**, as long as the sequenced operation does not name
the very mark the convergent one creates (a replica only names marks it has
seen, so a concurrent split never creates the mark a join or a table names).
This is what lets the client apply remote convergent operations without
re-validating its pending sequenced ones. -/
theorem convergent_preserves_sequenced_validity {s s' : Story} {c q : Op}
    (hc : c.convergent = true) (h : apply s c = some s') (hq : q.convergent = false)
    (hfresh : ∀ m, q.markRef = some m → c.newMark ≠ some m) :
    (apply s' q).isSome = (apply s q).isSome := by
  obtain ⟨ht, hv⟩ := convergent_effect hc h
  cases q with
  | insertChar _ _ | insertMark _ _ | deleteChar _ => simp [Op.convergent] at hq
  | join m =>
    have := hv m (hfresh m rfl)
    simp only [apply, ht, this]
    exact isSome_ite _ _ _
  | insertTable id anchor =>
    cases anchor with
    | none => simp only [apply, ht]; exact isSome_ite _ _ _
    | some m =>
      have := hv m (hfresh m rfl)
      simp only [apply, ht, this]
      exact isSome_ite _ _ _
  | deleteTable id => simp only [apply, ht]; exact isSome_ite _ _ _

/-- The side condition is needed: a join of a mark that does not exist yet is
rejected, and accepted once a split creates it. -/
theorem join_needs_existing_mark :
    (apply ⟨[], []⟩ (.join 1)).isSome = false ∧
      ((apply ⟨[], []⟩ (.insertMark 1 none)).bind (apply · (.join 1))).isSome = true := by
  decide

/-- The anchoring guard in action: a mark with a table after it cannot be
joined; once the table is deleted it can. -/
theorem join_blocked_by_table :
    ((apply ⟨[⟨1, .mark, true⟩], []⟩ (.insertTable 7 (some 1))).bind (apply · (.join 1))).isSome = false ∧
      (((apply ⟨[⟨1, .mark, true⟩], []⟩ (.insertTable 7 (some 1))).bind (apply · (.deleteTable 7))).bind
        (apply · (.join 1))).isSome = true := by
  decide

end JadeCollab.Anchor
