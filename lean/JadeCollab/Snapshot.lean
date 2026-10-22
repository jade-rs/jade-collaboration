/-! Session-scoped immutable read snapshots. The retained payload includes
the document, replication sequence and pending count captured together.
The finite Rust map has one entry per live session; stateful tests check
that lifecycle and compare actual payloads after concurrent editing. -/
namespace JadeCollab.Snapshot

abbrev Cache (α : Type) := Nat → Option (Nat × α)

def retain (cache : Cache α) (session token : Nat) (value : α) : Cache α :=
  fun owner => if owner = session then some (token, value) else cache owner

def lookup (cache : Cache α) (session token : Nat) : Option α :=
  match cache session with
  | none => none
  | some (saved, value) => if saved = token then some value else none

def remove (cache : Cache α) (session : Nat) : Cache α :=
  fun owner => if owner = session then none else cache owner

theorem retained_read (cache : Cache α) (session token : Nat) (value : α) :
    lookup (retain cache session token value) session token = some value := by
  simp [lookup, retain]

theorem other_session_unchanged (cache : Cache α) (session other token request : Nat)
    (value : α) (h : other ≠ session) :
    lookup (retain cache session token value) other request = lookup cache other request := by
  simp [lookup, retain, h]

theorem replaced_token_rejected (cache : Cache α) (session old fresh : Nat)
    (value : α) (h : fresh ≠ old) :
    lookup (retain cache session fresh value) session old = none := by
  simp [lookup, retain, h]

theorem removed_session_unreadable (cache : Cache α) (session token : Nat) :
    lookup (remove cache session) session token = none := by
  simp [lookup, remove]

structure Reader (α : Type) where
  live : α
  held : Cache α

def edit (reader : Reader α) (value : α) : Reader α := { reader with live := value }

/-- Editing the live replica cannot change a retained document or its
captured seq/pending metadata. -/
theorem live_edits_preserve_reads (reader : Reader α) (value : α) (session token : Nat) :
    lookup (edit reader value).held session token = lookup reader.held session token := rfl

end JadeCollab.Snapshot
