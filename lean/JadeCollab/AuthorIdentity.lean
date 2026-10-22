import JadeCollab.TextBoundary
import JadeCollab.NativeFailure

/-! Configured names are exact scalar sequences, never trimmed or rewritten.
Nonblank abstracts Rust Unicode trim; XML tokenization, native editing, IO and
watch/socket delivery are assumptions checked by Rust stateful/native tests.
This logical refusal transition runs only while healthy. Fatal native author
errors follow NativeFailure and cannot publish a refusal or subsequent edit. -/
namespace JadeCollab.AuthorIdentity

structure Name where
  scalars : List Nat
  nonblank : Bool
  deriving DecidableEq

def allowed (name : Name) : Bool := name.nonblank && TextBoundary.xmlText name.scalars

structure Grant where
  name : Name
  editor : Bool
  deriving DecidableEq

def load (table : List Grant) : Option (List Grant) :=
  if table.all (fun grant => allowed grant.name) then some table else none

def reload (old candidate : List Grant) : List Grant := (load candidate).getD old

theorem accepted_names_are_nonblank_xml (name : Name) (good : allowed name = true) :
    name.nonblank = true ∧ ∀ n ∈ name.scalars, TextBoundary.xmlScalar n = true := by
  simp only [allowed, Bool.and_eq_true] at good
  exact ⟨good.1, (TextBoundary.xml_text_admission_checks_every_scalar name.scalars).mp good.2⟩

theorem blank_names_are_refused (name : Name) (blank : name.nonblank = false) :
    allowed name = false := by simp [allowed, blank]

theorem admission_is_independent_of_role (name : Name) (a b : Bool) :
    load [⟨name,a⟩] = some [⟨name,a⟩] ↔ load [⟨name,b⟩] = some [⟨name,b⟩] := by simp [load]

theorem one_invalid_name_refuses_the_whole_table (table : List Grant) (bad : Grant)
    (present : bad ∈ table) (invalid : allowed bad.name = false) : load table = none := by
  have failed : table.all (fun grant => allowed grant.name) = false := by
    cases h : table.all (fun grant => allowed grant.name)
    · rfl
    · have good := (List.all_eq_true.mp h) bad present
      simp [invalid] at good
  simp [load, failed]

theorem invalid_reload_preserves_every_old_grant (old candidate : List Grant)
    (bad : Grant) (present : bad ∈ candidate) (invalid : allowed bad.name = false) :
    reload old candidate = old := by
  simp [reload, one_invalid_name_refuses_the_whole_table candidate bad present invalid]

theorem valid_reload_preserves_exact_names_and_roles (old candidate : List Grant)
    (valid : ∀ grant ∈ candidate, allowed grant.name = true) : reload old candidate = candidate := by
  simp [reload, load, List.all_eq_true.mpr valid]

inductive Entry (Op : Type) where
  | author (name : Name)
  | edit (name : Name) (op : Op)

structure Room (Op : Type) where
  history : List (Entry Op)
  named : Option Name
  processed : Nat

abbrev Verdict := NativeFailure.Verdict

def commit (room : Room Op) (ticket : Nat) (name : Name) (op : Op) : Room Op :=
  {history := room.history ++ (if room.named = some name then [] else [.author name]) ++ [.edit name op],
   named := some name, processed := ticket}

def submit (room : Room Op) (ticket : Nat) (name : Name) (op : Op)
    (permitted ready : Bool) : Room Op × Verdict :=
  if ticket ≤ room.processed then (room,.duplicate) else
  if permitted = false then ({room with processed := ticket},.refused) else
  if room.named = some name ∨ ready = true then (commit room ticket name op,.acknowledged)
  else ({room with processed := ticket},.refused)

theorem author_prerequisite_refusal_never_publishes_the_edit (room : Room Op) (ticket : Nat)
    (name : Name) (op : Op) (fresh : room.processed < ticket) (missing : room.named ≠ some name) :
    submit room ticket name op true false = ({room with processed := ticket}, .refused) := by
  simp [submit, Nat.not_le.mpr fresh, missing]

theorem viewer_refusal_preserves_authors_and_content (room : Room Op) (ticket : Nat)
    (name : Name) (op : Op) (ready : Bool) :
    (submit room ticket name op false ready).1.history = room.history ∧
    (submit room ticket name op false ready).1.named = room.named := by
  unfold submit
  split <;> simp

theorem old_tickets_cannot_register_authors_or_commit (room : Room Op) (ticket : Nat)
    (name : Name) (op : Op) (permitted ready : Bool) (old : ticket ≤ room.processed) :
    submit room ticket name op permitted ready = (room,.duplicate) := by simp [submit, old]

theorem a_refused_author_cannot_be_revived_with_a_changed_payload (room : Room Op)
    (ticket : Nat) (name changed : Name) (op replacement : Op)
    (fresh : room.processed < ticket) (missing : room.named ≠ some name) :
    submit (submit room ticket name op true false).1 ticket changed replacement true true =
      ((submit room ticket name op true false).1, .duplicate) := by
  rw [author_prerequisite_refusal_never_publishes_the_edit room ticket name op fresh missing]
  simp [submit]

theorem successful_edit_has_its_exact_registered_author (room : Room Op) (ticket : Nat)
    (name : Name) (op : Op) (fresh : room.processed < ticket) :
    (submit room ticket name op true true).1.named = some name ∧
    (submit room ticket name op true true).1.history = (commit room ticket name op).history := by
  simp [submit, commit, Nat.not_le.mpr fresh]

theorem known_author_needs_only_one_log_position (room : Room Op) (ticket : Nat)
    (name : Name) (op : Op) (fresh : room.processed < ticket) (known : room.named = some name) :
    (submit room ticket name op true false).1.history = room.history ++ [.edit name op] := by
  simp [submit, commit, Nat.not_le.mpr fresh, known]

theorem a_new_author_is_ordered_before_its_edit (room : Room Op) (ticket : Nat)
    (name : Name) (op : Op) (fresh : room.processed < ticket) (missing : room.named ≠ some name) :
    (submit room ticket name op true true).1.history = room.history ++ [.author name, .edit name op] := by
  simp [submit, commit, Nat.not_le.mpr fresh, missing, List.append_assoc]

/-! Attribute tokens after XML line-ending normalization. A numeric/named
reference is decoded once; resulting characters are never parsed again. Rust
checks the lexical parser, literal CRLF normalization and package preservation. -/
inductive AttrToken where
  | literal (scalar : Nat)
  | reference (scalar : Nat)
  deriving DecidableEq

def decodeToken : AttrToken → Nat
  | .literal n => if n = 9 ∨ n = 10 ∨ n = 13 then 32 else n
  | .reference n => n

def encodeToken (n : Nat) : AttrToken :=
  if n = 9 ∨ n = 10 ∨ n = 13 then .reference n else .literal n

def decode (tokens : List AttrToken) : List Nat := tokens.map decodeToken

def encode (text : List Nat) : List AttrToken := text.map encodeToken

theorem encoded_scalars_roundtrip (n : Nat) : decodeToken (encodeToken n) = n := by
  unfold encodeToken
  split <;> simp_all [decodeToken]

theorem encoded_names_roundtrip_without_loss (text : List Nat) : decode (encode text) = text := by
  simp [decode, encode, List.map_map, Function.comp_def, encoded_scalars_roundtrip]

theorem literal_attribute_whitespace_normalizes :
    decode [.literal 9, .literal 10, .literal 13] = [32,32,32] := by decide

theorem referenced_attribute_whitespace_is_preserved :
    decode [.reference 9, .reference 10, .reference 13] = [9,10,13] := by decide

theorem entity_shaped_names_are_not_recursively_decoded :
    decode [.reference 38, .literal 97, .literal 109, .literal 112, .literal 59] = [38,97,109,112,59] := by decide

end JadeCollab.AuthorIdentity
