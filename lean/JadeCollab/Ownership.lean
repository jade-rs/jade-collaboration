/-! Replica resumption is authorized by a trusted principal before socket or
presence replacement. Principal numbers abstract stable configured names;
None is anonymous access. This specifies ownership, not credential validation. -/
namespace JadeCollab.Ownership

structure Room (S : Type) where
  owners : Nat → Option (Option Nat)
  document : S
  processed : Nat → Nat
  sockets : Nat → Nat
  presence : Nat → Bool

def authorize (owners : Nat → Option (Option Nat)) (replica : Nat)
    (caller : Option Nat) : Bool :=
  match owners replica with
  | none => false
  | some owner => decide (owner = caller)

def resume (room : Room S) (replica socket : Nat) (caller : Option Nat) : Option (Room S) :=
  if authorize room.owners replica caller then
    some {room with
      sockets := fun id => if id = replica then socket else room.sockets id
      presence := fun id => if id = replica then false else room.presence id}
  else none

def handle (room : Room S) (replica socket : Nat) (caller : Option Nat) : Room S :=
  (resume room replica socket caller).getD room

theorem unknown_replica_is_refused (room : Room S) (replica socket : Nat)
    (caller : Option Nat) (h : room.owners replica = none) :
    resume room replica socket caller = none := by simp [resume, authorize, h]

theorem different_principal_is_refused (room : Room S) (replica socket : Nat)
    (owner caller : Option Nat) (h : room.owners replica = some owner)
    (different : owner ≠ caller) : resume room replica socket caller = none := by
  simp [resume, authorize, h, different]

theorem refused_resume_preserves_everything (room : Room S) (replica socket : Nat)
    (caller : Option Nat) (h : authorize room.owners replica caller = false) :
    handle room replica socket caller = room := by simp [handle, resume, h]

theorem same_principal_replaces_only_its_socket_and_presence (room : Room S)
    (replica socket : Nat) (caller : Option Nat) (h : room.owners replica = some caller) :
    (handle room replica socket caller).sockets replica = socket ∧
    (handle room replica socket caller).presence replica = false := by
  simp [handle, resume, authorize, h]

theorem resume_preserves_document_and_tickets (room : Room S) (replica socket : Nat)
    (caller : Option Nat) :
    (handle room replica socket caller).document = room.document ∧
    (handle room replica socket caller).processed = room.processed ∧
    (handle room replica socket caller).owners = room.owners := by
  unfold handle resume
  split <;> simp

theorem other_connections_are_unchanged (room : Room S) (replica other socket : Nat)
    (caller : Option Nat) (different : other ≠ replica) :
    (handle room replica socket caller).sockets other = room.sockets other ∧
    (handle room replica socket caller).presence other = room.presence other := by
  unfold handle resume
  split <;> simp [different]

theorem authenticated_caller_cannot_claim_anonymous_or_legacy_replica
    (room : Room S) (replica socket principal : Nat)
    (h : room.owners replica = some none) :
    resume room replica socket (some principal) = none := by
  simp [resume, authorize, h]

end JadeCollab.Ownership
