import Std

/-! Ephemeral DOCX cursors over one story's canonical RGA walk. Tombstones
retain IDs. A known right endpoint wins; otherwise use one past the left,
or the story end when both are absent. Unknown/reversed endpoints are hidden.
Projection is a raw-prefix fold, independent of Rust's paragraph enumeration.

Canonical RGA order and unique IDs are supplied by the existing RGA model.
Widths abstract UTF-8 byte lengths; Rust PBT checks actual Unicode, story
surfaces, optimistic replicas and serialized checkpoints. Word/browser layout,
focus events and the Rust compiler remain outside this specification. -/
namespace JadeCollab.TextCursor

inductive Kind where
  | character (bytes : Nat)
  | mark
  | anchor
  deriving DecidableEq, Repr
structure Atom where
  id : Nat
  kind : Kind
  visible : Bool
  deriving DecidableEq, Repr
structure Cursor where
  left : Option Nat
  right : Option Nat
  deriving DecidableEq, Repr
structure Position where
  paragraph : Nat
  offset : Nat
  deriving DecidableEq, Repr

def lookupRef (ids : List Nat) : Option Nat → Option (Option Nat)
  | none => some none
  | some id => (ids.findIdx? (· == id)).map some

def cutAt (length : Nat) : Option Nat → Option Nat → Option Nat
  | some left, some right => if left < right then some right else none
  | none, some right => some right
  | some left, none => some (left + 1)
  | none, none => some length

def cut (ids : List Nat) (cursor : Cursor) : Option Nat :=
  match lookupRef ids cursor.left, lookupRef ids cursor.right with
  | some left, some right => cutAt ids.length left right
  | _, _ => none

def step (position : Position) (atom : Atom) : Position :=
  if atom.visible then match atom.kind with
    | .character bytes => ⟨position.paragraph, position.offset + bytes⟩
    | .mark => ⟨position.paragraph + 1, 0⟩
    | .anchor => position
  else position

def project (atoms : List Atom) : Position := atoms.foldl step ⟨0, 0⟩

def resolve (rendered : Bool) (atoms : List Atom) (cursor : Cursor) : Option Position :=
  if rendered then (cut (atoms.map (·.id)) cursor).map fun n => project (atoms.take n)
  else none

def hide (ids : List Nat) (atoms : List Atom) : List Atom :=
  atoms.map fun atom => {atom with visible := atom.visible && !(ids.contains atom.id)}

theorem hidden_story_has_no_cursor (atoms : List Atom) (cursor : Cursor) :
    resolve false atoms cursor = none := rfl

theorem unknown_left_defers (ids : List Nat) (id : Nat) (right : Option Nat)
    (unknown : ids.findIdx? (· == id) = none) : cut ids ⟨some id, right⟩ = none := by
  simp [cut, lookupRef, unknown]

theorem unknown_right_defers (ids : List Nat) (left : Option Nat) (id : Nat)
    (unknown : ids.findIdx? (· == id) = none) : cut ids ⟨left, some id⟩ = none := by
  simp [cut, lookupRef, unknown]

theorem ordered_right_endpoint_wins (length left right : Nat) (ordered : left < right) :
    cutAt length (some left) (some right) = some right := by simp [cutAt, ordered]

theorem reversed_or_equal_endpoints_are_refused (length left right : Nat) (bad : right ≤ left) :
    cutAt length (some left) (some right) = none := by simp [cutAt]; omega

theorem missing_right_uses_one_past_left (length left : Nat) :
    cutAt length (some left) none = some (left + 1) := rfl

theorem absent_endpoints_use_story_end (ids : List Nat) :
    cut ids ⟨none, none⟩ = some ids.length := rfl

theorem cutAt_is_bounded (length : Nat) (left right : Option Nat) (n : Nat)
    (hl : ∀ i, left = some i → i < length)
    (hr : ∀ i, right = some i → i < length)
    (found : cutAt length left right = some n) : n ≤ length := by
  cases left <;> cases right <;> simp_all [cutAt] <;> omega

theorem lookupRef_in_bounds (ids : List Nat) (ref : Option Nat) (i : Nat)
    (found : lookupRef ids ref = some (some i)) : i < ids.length := by
  cases ref with
  | none => simp [lookupRef] at found
  | some id =>
    simp only [lookupRef, Option.map_eq_some_iff] at found
    obtain ⟨j, hj, heq⟩ := found
    cases heq
    exact (List.findIdx?_eq_some_iff_findIdx_eq.mp hj).1

theorem cut_is_bounded (ids : List Nat) (cursor : Cursor) (n : Nat)
    (found : cut ids cursor = some n) : n ≤ ids.length := by
  cases hl : lookupRef ids cursor.left with
  | none => simp [cut, hl] at found
  | some left =>
    cases hr : lookupRef ids cursor.right with
    | none => simp [cut, hr] at found
    | some right =>
      simp only [cut, hl, hr] at found
      exact cutAt_is_bounded _ _ _ _
        (fun i hi => lookupRef_in_bounds ids cursor.left i (hi ▸ hl))
        (fun i hi => lookupRef_in_bounds ids cursor.right i (hi ▸ hr)) found

theorem deletion_keeps_the_identifier_spine (ids : List Nat) (atoms : List Atom) :
    (hide ids atoms).map (·.id) = atoms.map (·.id) := by
  simp [hide, List.map_map, Function.comp_def]

theorem deleting_even_both_neighbours_keeps_the_raw_cut (ids : List Nat)
    (atoms : List Atom) (cursor : Cursor) :
    cut ((hide ids atoms).map (·.id)) cursor = cut (atoms.map (·.id)) cursor := by
  rw [deletion_keeps_the_identifier_spine]

theorem deletion_is_idempotent (ids : List Nat) (atoms : List Atom) :
    hide ids (hide ids atoms) = hide ids atoms := by
  simp only [hide, List.map_map]
  congr 1; funext atom
  dsimp [Function.comp_def]
  cases atom with
  | mk id kind visible => cases visible <;> cases ids.contains id <;> rfl

theorem deletion_order_does_not_change_cursor_resolution (a b : List Nat)
    (atoms : List Atom) (cursor : Cursor) (rendered : Bool) :
    resolve rendered (hide a (hide b atoms)) cursor =
      resolve rendered (hide b (hide a atoms)) cursor := by
  have comm : hide a (hide b atoms) = hide b (hide a atoms) := by
    simp only [hide, List.map_map]
    congr 1; funext atom
    dsimp [Function.comp_def]
    cases atom with
    | mk id kind visible => cases visible <;> cases a.contains id <;> cases b.contains id <;> rfl
  rw [comm]

theorem tombstones_have_zero_geometry (p : Position) (id : Nat) (kind : Kind) :
    step p ⟨id, kind, false⟩ = p := rfl

theorem zero_width_anchors_have_zero_geometry (p : Position) (id : Nat) (visible : Bool) :
    step p ⟨id, .anchor, visible⟩ = p := by cases visible <;> rfl

theorem a_visible_mark_advances_paragraph_and_resets_offset (p : Position) (id : Nat) :
    step p ⟨id, .mark, true⟩ = ⟨p.paragraph + 1, 0⟩ := rfl

theorem utf8_width_is_added_exactly (p : Position) (id bytes : Nat) :
    step p ⟨id, .character bytes, true⟩ = ⟨p.paragraph, p.offset + bytes⟩ := rfl

theorem projection_composes_across_prefixes (head suffix : List Atom) :
    project (head ++ suffix) = suffix.foldl step (project head) := by
  simp [project, List.foldl_append]

theorem equal_cuts_ignore_suffix_geometry (head a b : List Atom) (cursor : Cursor)
    (ha : cut ((head ++ a).map (·.id)) cursor = some head.length)
    (hb : cut ((head ++ b).map (·.id)) cursor = some head.length) :
    resolve true (head ++ a) cursor = resolve true (head ++ b) cursor := by
  unfold resolve
  rw [ha, hb]
  simp

def bytes (atom : Atom) : Nat :=
  if atom.visible then match atom.kind with | .character n => n | _ => 0 else 0

theorem projection_offset_is_bounded_by_visible_utf8_bytes (atoms : List Atom) (p : Position) :
    (atoms.foldl step p).offset ≤ p.offset + (atoms.map bytes).sum := by
  induction atoms generalizing p with
  | nil => simp
  | cons atom rest ih =>
    have next : (step p atom).offset ≤ p.offset + bytes atom := by
      cases atom with
      | mk id kind visible => cases visible <;> cases kind <;> simp [step, bytes]
    have tail := ih (step p atom)
    simp only [List.foldl_cons, List.map_cons, List.sum_cons]
    omega

-- Filtering out tombstones would lose otherwise valid endpoint identities.
theorem filtering_tombstones_would_lose_a_valid_cursor :
    resolve true [⟨1,.character 1,true⟩, ⟨2,.character 4,false⟩,
      ⟨3,.character 3,false⟩, ⟨4,.character 1,true⟩] ⟨some 2,some 3⟩ = some ⟨0,1⟩ ∧
    resolve true [⟨1,.character 1,true⟩, ⟨4,.character 1,true⟩] ⟨some 2,some 3⟩ = none := by
  decide

-- Lexicographic document order accounts for offset reset at a paragraph mark.
def Precedes (a b : Position) : Prop :=
  a.paragraph < b.paragraph ∨ (a.paragraph = b.paragraph ∧ a.offset ≤ b.offset)

theorem precedes_trans (a b c : Position) (hab : Precedes a b) (hbc : Precedes b c) :
    Precedes a c := by unfold Precedes at *; omega

theorem one_atom_never_moves_geometry_backwards (p : Position) (atom : Atom) :
    Precedes p (step p atom) := by
  cases atom with
  | mk id kind visible => cases visible <;> cases kind <;> simp [step, Precedes]

theorem a_prefix_fold_never_moves_geometry_backwards (p : Position) (atoms : List Atom) :
    Precedes p (atoms.foldl step p) := by
  induction atoms generalizing p with
  | nil => simp [Precedes]
  | cons atom rest ih =>
    exact precedes_trans _ _ _ (one_atom_never_moves_geometry_backwards p atom) (ih _)

theorem ordered_raw_cuts_preserve_selection_direction (atoms : List Atom) (left right : Nat)
    (ordered : left ≤ right) : Precedes (project (atoms.take left)) (project (atoms.take right)) := by
  have split : atoms.take right = atoms.take left ++ (atoms.drop left).take (right - left) := by
    rw [← List.take_add]
    congr 1
    omega
  rw [split, projection_composes_across_prefixes]
  exact a_prefix_fold_never_moves_geometry_backwards _ _

end JadeCollab.TextCursor
