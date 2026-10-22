import Std

/-! Bounded counters and validation for checkpoint/tail/journal restoration.
IO, serde and materialization are abstracted; Rust stateful and native restart
tests check the executable implementations. This does not specify reservation
of a cause plus its derived operations at the live log boundary. -/
namespace JadeCollab.RecoveryBounds

def issue (limit next : Nat) : Option (Nat × Nat) :=
  if 0 < next ∧ next < limit then some (next, next + 1) else none

theorem issued_id_is_nonzero_and_has_a_successor (limit next id following : Nat)
    (h : issue limit next = some (id, following)) :
    0 < id ∧ id = next ∧ following = id + 1 ∧ following ≤ limit := by
  unfold issue at h
  split at h <;> simp_all <;> omega

theorem exhausted_counter_refuses (limit : Nat) : issue limit limit = none := by simp [issue]

theorem issued_ids_never_repeat (limit next id following other after : Nat)
    (h : issue limit next = some (id, following))
    (h' : issue limit following = some (other, after)) : id < other := by
  have := issued_id_is_nonzero_and_has_a_successor limit next id following h
  have := issued_id_is_nonzero_and_has_a_successor limit following other after h'
  omega

def replayTicket (processed ticket : Nat) : Option Nat :=
  if processed < ticket then some ticket else none

def afterTicket (processed ticket : Nat) : Nat := (replayTicket processed ticket).getD processed

theorem accepted_replay_strictly_advances (processed ticket accepted : Nat)
    (h : replayTicket processed ticket = some accepted) : processed < accepted ∧ accepted = ticket := by
  unfold replayTicket at h
  split at h <;> simp_all

theorem repeated_or_older_ticket_is_refused (processed ticket : Nat) (h : ticket ≤ processed) :
    replayTicket processed ticket = none ∧ afterTicket processed ticket = processed := by
  simp [replayTicket, afterTicket, Nat.not_lt.mpr h]

theorem replay_does_not_rewind_processed (processed ticket : Nat) : processed ≤ afterTicket processed ticket := by
  unfold afterTicket replayTicket
  split <;> simp_all <;> omega

def tailPositions (start count : Nat) : List Nat := (List.range count).map (fun i => start + i + 1)

theorem empty_tail_at_limit_needs_no_successor (limit : Nat) : tailPositions limit 0 = [] := rfl

theorem retained_tail_positions_fit_checkpoint (start count checkpoint position : Nat)
    (ends : start + count = checkpoint) (member : position ∈ tailPositions start count) :
    start < position ∧ position ≤ checkpoint := by
  simp only [tailPositions, List.mem_map] at member
  obtain ⟨i, hi, rfl⟩ := member
  have : i < count := List.mem_range.mp hi
  omega

theorem retained_tail_positions_fit_limit (start count checkpoint limit position : Nat)
    (ends : start + count = checkpoint) (bounded : checkpoint ≤ limit)
    (member : position ∈ tailPositions start count) : position ≤ limit := by
  have := retained_tail_positions_fit_checkpoint start count checkpoint position ends member
  omega

def successor (limit current : Nat) : Option Nat :=
  if current < limit then some (current + 1) else none

theorem full_log_has_no_successor (limit : Nat) : successor limit limit = none := by simp [successor]

theorem accepted_successor_does_not_wrap (limit current next : Nat)
    (h : successor limit current = some next) : current < next ∧ next ≤ limit := by
  unfold successor at h
  split at h <;> simp_all <;> omega

end JadeCollab.RecoveryBounds
