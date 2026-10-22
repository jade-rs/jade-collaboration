import Std

/-! Incremental geometry for one unchanged paragraph topology. UTF-8 widths
and the actual RGA predecessor determine the insertion point. The Rust index
uses the canonical document walk when cold, disables itself for structural
operations and ambiguous visible IDs, and is absent from snapshots/clones.
This specification abstracts maps, mutexes, RGA lookup and native realization;
Rust stateful PBT compares accelerated effects with the canonical walk. -/
namespace JadeCollab.TextPositions

-- Native anchor-aware joins use a temporary atom after the first paragraph's
-- trailing references. Remove that one position after concatenating the runs;
-- existing user atoms equal to the placeholder remain ordinary content.
def cleanupPlaceholderJoin {α : Type} (left right : List α) (placeholder : α) : List α :=
  (left ++ placeholder :: right).take left.length ++
    (left ++ placeholder :: right).drop (left.length + 1)

theorem placeholder_join_restores_exact_atom_order {α : Type} (left right : List α) (placeholder : α) :
    cleanupPlaceholderJoin left right placeholder = left ++ right := by
  simp [cleanupPlaceholderJoin, List.drop_append]

theorem placeholder_join_preserves_anchor_projection {α : Type} (left right : List α)
    (placeholder : α) (isAnchor : α → Bool) :
    (cleanupPlaceholderJoin left right placeholder).filter isAnchor =
      left.filter isAnchor ++ right.filter isAnchor := by
  rw [placeholder_join_restores_exact_atom_order]
  simp

theorem matching_user_characters_are_never_removed {α : Type} (left right : List α)
    (placeholder : α) (user : placeholder ∈ left ++ right) :
    placeholder ∈ cleanupPlaceholderJoin left right placeholder := by
  rw [placeholder_join_restores_exact_atom_order]
  exact user

structure Location where
  paragraph : Nat
  offset : Nat
  width : Nat
  anchors : Nat
  deriving DecidableEq

inductive Kind where
  | character | anchor | mark
  deriving DecidableEq

def follows (kind : Kind) (entry : Location) (offset anchors : Nat) : Bool :=
  match kind with
  | .anchor => decide (offset < entry.offset ∨ (entry.offset = offset ∧ anchors < entry.anchors))
  | .character | .mark => decide (offset ≤ entry.offset)

def shift (kind : Kind) (entry : Location) (paragraph offset anchors bytes : Nat) : Location :=
  if entry.paragraph = paragraph ∧ follows kind entry offset anchors = true then
    {entry with offset := entry.offset + bytes}
  else entry

theorem another_paragraph_is_never_repaired (kind : Kind) (entry : Location)
    (paragraph offset anchors bytes : Nat) (other : entry.paragraph ≠ paragraph) :
    shift kind entry paragraph offset anchors bytes = entry := by simp [shift, other]

theorem character_before_the_insertion_keeps_its_location (entry : Location)
    (paragraph offset anchors bytes : Nat) (before : entry.offset < offset) :
    shift .character entry paragraph offset anchors bytes = entry := by
  simp [shift, follows, Nat.not_le.mpr before]

theorem a_positive_width_prefix_character_is_not_shifted (entry : Location)
    (paragraph offset anchors bytes : Nat) (positive : 0 < entry.width)
    (before : entry.offset + entry.width ≤ offset) :
    shift .character entry paragraph offset anchors bytes = entry := by
  apply character_before_the_insertion_keeps_its_location
  omega

theorem suffix_character_moves_by_exactly_the_inserted_utf8_bytes (entry : Location)
    (offset anchors bytes : Nat) (after : offset ≤ entry.offset) :
    shift .character entry entry.paragraph offset anchors bytes = {entry with offset := entry.offset + bytes} := by
  simp [shift, follows, after]

theorem paragraph_end_moves_even_when_inserting_at_its_old_offset (entry : Location)
    (anchors bytes : Nat) :
    shift .mark entry entry.paragraph entry.offset anchors bytes = {entry with offset := entry.offset + bytes} := by
  simp [shift, follows]

theorem equal_offset_anchors_before_the_caret_do_not_move (entry : Location)
    (paragraph anchors bytes : Nat) (before : entry.anchors ≤ anchors) :
    shift .anchor entry paragraph entry.offset anchors bytes = entry := by
  simp [shift, follows, Nat.not_lt.mpr before]

theorem equal_offset_anchors_after_the_caret_move (entry : Location)
    (anchors bytes : Nat) (after : anchors < entry.anchors) :
    shift .anchor entry entry.paragraph entry.offset anchors bytes = {entry with offset := entry.offset + bytes} := by
  simp [shift, follows, after]

theorem anchor_after_the_caret_moves (entry : Location) (offset anchors bytes : Nat)
    (after : offset < entry.offset) :
    shift .anchor entry entry.paragraph offset anchors bytes = {entry with offset := entry.offset + bytes} := by
  simp [shift, follows, after]

theorem shifting_never_changes_width_anchor_count_or_paragraph (kind : Kind) (entry : Location)
    (paragraph offset anchors bytes : Nat) :
    (shift kind entry paragraph offset anchors bytes).width = entry.width ∧
    (shift kind entry paragraph offset anchors bytes).anchors = entry.anchors ∧
    (shift kind entry paragraph offset anchors bytes).paragraph = entry.paragraph := by
  unfold shift
  split <;> simp

def newCharacter (paragraph offset anchors prefixBytes width : Nat) : Location :=
  ⟨paragraph, offset + prefixBytes, width, anchors⟩

theorem inserted_run_uses_utf8_prefixes_without_adding_anchors
    (paragraph offset anchors : Nat) (widths : List Nat) (width : Nat) :
    newCharacter paragraph offset anchors widths.sum width =
      ⟨paragraph, offset + widths.sum, width, anchors⟩ := rfl

theorem inserting_a_run_then_its_successor_adds_byte_widths
    (offset : Nat) (a b : List Nat) : offset + a.sum + b.sum = offset + (a ++ b).sum := by
  simp [List.sum_append, Nat.add_assoc]

structure State (D I : Type) where
  document : D
  cache : Option I

-- Cache contents are runtime data and never occur in the durable projection.
def snapshot (state : State D I) : D := state.document

def discard (state : State D I) : State D I := {state with cache := none}

def lookup (derive : D → I) (state : State D I) : I := state.cache.getD (derive state.document)

theorem discarding_an_index_preserves_the_document (state : State D I) :
    snapshot (discard state) = snapshot state := rfl

theorem a_cold_lookup_uses_the_current_canonical_projection (derive : D → I) (state : State D I) :
    lookup derive (discard state) = derive state.document := rfl

theorem a_valid_warm_index_agrees_with_the_canonical_projection (derive : D → I)
    (state : State D I) (valid : state.cache = some (derive state.document)) :
    lookup derive state = derive state.document := by simp [lookup, valid]

def structural (edit : D → D) (state : State D I) : State D I :=
  ⟨edit state.document, none⟩

theorem structural_edits_never_reuse_old_positions (derive : D → I) (edit : D → D) (state : State D I) :
    lookup derive (structural edit state) = derive (edit state.document) := rfl

-- The fallible insertion runs before any cold geometry construction. On
-- refusal the previous warm/cold state is restored with no projection work.
def attempt (derive : D → I) (repair : I → D → I) (state : State D I)
    (inserted : Option D) : State D I × Nat :=
  match inserted with
  | none => (state, 0)
  | some document => match state.cache with
    | none => (⟨document, some (derive document)⟩, 1)
    | some index => (⟨document, some (repair index document)⟩, 0)

theorem a_refused_insertion_preserves_document_and_index (derive : D → I)
    (repair : I → D → I) (state : State D I) : (attempt derive repair state none).1 = state := rfl

theorem a_refused_insertion_needs_no_canonical_projection (derive : D → I)
    (repair : I → D → I) (state : State D I) : (attempt derive repair state none).2 = 0 := rfl

-- Count canonical full projections in a consecutive committed text segment.
def scans (warm : Bool) : Nat → Nat
  | 0 => 0
  | edits + 1 => (if warm then 0 else 1) + scans true edits

theorem warm_committed_typing_needs_no_full_projection (edits : Nat) : scans true edits = 0 := by
  induction edits with
  | zero => rfl
  | succ edits ih => simpa [scans] using ih

theorem consecutive_committed_typing_needs_at_most_one_full_projection (warm : Bool) (edits : Nat) :
    scans warm edits ≤ 1 := by
  cases edits with
  | zero => simp [scans]
  | succ edits => cases warm <;> simp [scans, warm_committed_typing_needs_no_full_projection]

end JadeCollab.TextPositions
