import Std

/-! Scalar admission for directly written XML versus SpreadsheetML xstrings.
Numbers abstract the native finite-number predicate. Formula lists are supplied
after prefix/empty-whitespace normalization, checked by Rust tests. Native
serialization/escaping and successful materialization are assumptions: Rust
stateful tests export/reopen real files after every transition. -/
namespace JadeCollab.TextBoundary

def xmlScalar (n : Nat) : Bool := decide (
  n = 9 ∨ n = 10 ∨ n = 13 ∨
  (32 ≤ n ∧ n ≤ 55295) ∨ (57344 ≤ n ∧ n ≤ 65533) ∨ (65536 ≤ n ∧ n ≤ 1114111))

def xmlText (text : List Nat) : Bool := text.all xmlScalar

inductive Payload where
  | xml (text : List Nat)
  | xstring (text : List Nat)
  | formula (normalized : List Nat)
  | number (finite : Bool)
  deriving DecidableEq

def allowed : Payload → Bool
  | .xml text => xmlText text
  | .xstring _ => true
  | .formula text => !text.isEmpty && xmlText text
  | .number finite => finite

theorem xml_text_admission_checks_every_scalar (text : List Nat) :
    xmlText text = true ↔ ∀ n ∈ text, xmlScalar n = true := by
  simp [xmlText, List.all_eq_true]

theorem c0_controls_except_xml_whitespace_are_refused (n : Nat)
    (small : n < 32) (notTab : n ≠ 9) (notLF : n ≠ 10) (notCR : n ≠ 13) :
    xmlScalar n = false := by simp [xmlScalar]; omega

theorem xml_whitespace_is_preserved : xmlScalar 9 = true ∧ xmlScalar 10 = true ∧ xmlScalar 13 = true := by decide

theorem surrogate_numbers_are_not_xml_scalars (n : Nat)
    (lo : 55296 ≤ n) (hi : n ≤ 57343) : xmlScalar n = false := by
  simp [xmlScalar]; omega

theorem forbidden_bmp_endpoints_are_refused : xmlScalar 65534 = false ∧ xmlScalar 65535 = false := by decide

theorem supplementary_noncharacters_are_still_xml_characters :
    xmlScalar 131070 = true ∧ xmlScalar 1114111 = true := by decide

theorem concatenation_is_admitted_exactly_when_both_sides_are (a b : List Nat) :
    xmlText (a ++ b) = (xmlText a && xmlText b) := by simp [xmlText]

theorem one_forbidden_scalar_refuses_plain_xml (text : List Nat) (n : Nat)
    (present : n ∈ text) (bad : xmlScalar n = false) : allowed (.xml text) = false := by
  cases h : xmlText text
  · exact h
  · have good := (xml_text_admission_checks_every_scalar text).mp h n present
    simp [bad] at good

theorem spreadsheet_text_uses_its_lossless_escape_even_for_forbidden_scalars (text : List Nat) :
    allowed (.xstring text) = true := rfl

theorem normalized_empty_formula_is_refused : allowed (.formula []) = false := by decide

theorem forbidden_formula_text_is_refused (text : List Nat) (bad : xmlText text = false) :
    allowed (.formula text) = false := by simp [allowed, bad]

theorem nonfinite_numbers_are_refused : allowed (.number false) = false := rfl

structure Room where
  committed : List Payload := []
  native : List Payload := []
  processed : Nat := 0
  deriving DecidableEq

def submit (ticket : Nat) (payload : Payload) (room : Room) : Room :=
  if ticket ≤ room.processed then room else
  if allowed payload then
    {committed := room.committed ++ [payload], native := room.native ++ [payload], processed := ticket}
  else {room with processed := ticket}

theorem refused_payload_preserves_model_and_native_state (ticket : Nat) (payload : Payload)
    (room : Room) (bad : allowed payload = false) :
    (submit ticket payload room).committed = room.committed ∧
    (submit ticket payload room).native = room.native := by
  unfold submit
  split <;> simp [bad]

theorem duplicate_ticket_preserves_everything (ticket : Nat) (payload : Payload) (room : Room)
    (old : ticket ≤ room.processed) : submit ticket payload room = room := by simp [submit, old]

theorem admission_preserves_a_valid_history (ticket : Nat) (payload : Payload) (room : Room)
    (valid : ∀ p ∈ room.committed, allowed p = true) :
    ∀ p ∈ (submit ticket payload room).committed, allowed p = true := by
  unfold submit
  split
  · exact valid
  · split
    · rename_i good
      simp_all only [List.mem_append, List.mem_singleton]
      intro p present
      rcases present with previous | same
      · exact valid p previous
      · simpa [same] using good
    · exact valid

theorem successful_lossless_materialization_preserves_agreement (ticket : Nat)
    (payload : Payload) (room : Room) (agree : room.native = room.committed) :
    (submit ticket payload room).native = (submit ticket payload room).committed := by
  unfold submit
  split
  · exact agree
  · split <;> simp [agree]

end JadeCollab.TextBoundary
