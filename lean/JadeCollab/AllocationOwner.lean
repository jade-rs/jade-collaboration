import Std

/-! New IDs/write stamps are owned by the submitting replica. Target/anchor
references remain shared. Native formats extract `fresh`; Rust field matrices
and stateful tests check that extraction, including unused merge sentinels.
The state abstracts one replica's ticket watermark and successful application;
parsing, connection authorization and native admission are separate boundaries. -/
namespace JadeCollab.AllocationOwner

structure Op where
  fresh : List Nat
  references : List Nat := []
  tracked : Bool := false
  deriving DecidableEq

def allowed (origin : Nat) (op : Op) : Bool := op.fresh.all (· == origin)

structure Room where
  processed : Nat := 0
  document : List Op := []
  authorRegistered : Bool := false
  logPosition : Nat := 0
  deriving DecidableEq

def submit (origin ticket : Nat) (op : Op) (room : Room) : Room :=
  if ticket ≤ room.processed then room else
  let advanced := { room with processed := ticket }
  if allowed origin op then
    { advanced with
      document := room.document ++ [op]
      authorRegistered := room.authorRegistered || op.tracked
      logPosition := room.logPosition + 1 + (if op.tracked && !room.authorRegistered then 1 else 0) }
  else advanced

theorem admission_checks_every_fresh_owner (origin : Nat) (op : Op) :
    allowed origin op = true ↔ ∀ owner ∈ op.fresh, owner = origin := by
  simp [allowed, List.all_eq_true]

theorem one_forged_owner_is_refused (origin forged : Nat) (op : Op)
    (present : forged ∈ op.fresh) (different : forged ≠ origin) :
    allowed origin op = false := by
  cases h : allowed origin op
  · rfl
  · have same := (admission_checks_every_fresh_owner origin op).mp h forged present
    exact False.elim (different same)

theorem shared_references_do_not_change_admission (origin : Nat) (op : Op)
    (references : List Nat) :
    allowed origin {op with references := references} = allowed origin op := rfl

theorem operations_without_allocations_need_no_namespace (origin : Nat) (op : Op) :
    allowed origin {op with fresh := []} = true := by simp [allowed]

theorem owned_runs_are_allowed (origin count : Nat) (op : Op) :
    allowed origin {op with fresh := List.replicate count origin} = true := by
  simp [allowed]

theorem origin_namespace_cannot_be_minted_by_a_client (origin : Nat)
    (op : Op) (nonzero : origin ≠ 0) :
    allowed origin {op with fresh := 0 :: op.fresh} = false := by
  simp [allowed, Ne.symm nonzero]

theorem duplicate_ticket_has_no_effect (origin ticket : Nat) (op : Op) (room : Room)
    (old : ticket ≤ room.processed) : submit origin ticket op room = room := by
  simp [submit, old]

theorem namespace_refusal_preserves_file_model_log_and_author
    (origin ticket : Nat) (op : Op) (room : Room) (refused : allowed origin op = false) :
    (submit origin ticket op room).document = room.document ∧
    (submit origin ticket op room).logPosition = room.logPosition ∧
    (submit origin ticket op room).authorRegistered = room.authorRegistered := by
  unfold submit
  split <;> simp [refused]

theorem refused_new_ticket_is_processed (origin ticket : Nat) (op : Op) (room : Room)
    (newTicket : room.processed < ticket) (refused : allowed origin op = false) :
    (submit origin ticket op room).processed = ticket := by
  simp [submit, Nat.not_le.mpr newTicket, refused]

theorem changed_payload_cannot_revive_a_refused_ticket
    (origin ticket : Nat) (bad corrected : Op) (room : Room)
    (newTicket : room.processed < ticket) (refused : allowed origin bad = false) :
    submit origin ticket corrected (submit origin ticket bad room) = submit origin ticket bad room := by
  apply duplicate_ticket_has_no_effect
  rw [refused_new_ticket_is_processed origin ticket bad room newTicket refused]
  exact Nat.le_refl ticket

theorem accepted_submission_publishes_only_owned_allocations
    (origin ticket : Nat) (op : Op) (room : Room)
    (newTicket : room.processed < ticket) (accepted : allowed origin op = true) :
    (submit origin ticket op room).document = room.document ++ [op] ∧
    ∀ owner ∈ op.fresh, owner = origin := by
  constructor
  · simp [submit, Nat.not_le.mpr newTicket, accepted]
  · exact (admission_checks_every_fresh_owner origin op).mp accepted

-- Admission is intentionally absent from trusted replay/origin facts, so a
-- policy upgrade cannot reinterpret content already committed in old history.
def recorded (op : Op) (room : Room) : Room :=
  {room with document := room.document ++ [op], logPosition := room.logPosition + 1}

theorem recorded_facts_keep_their_original_content (op : Op) (room : Room) :
    (recorded op room).document = room.document ++ [op] := rfl

end JadeCollab.AllocationOwner
