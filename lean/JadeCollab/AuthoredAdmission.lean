import Std

/-! Preflight of a requested author prerequisite and its tracked edit. A
matching author needs one log position; a new author needs two. Logical
refusals publish neither. Native failures after publication use NativeFailure
instead; this model assumes successful native application and healthy journal
IO. Rust checks real model guards, exact contexts and native socket recovery. -/
namespace JadeCollab.AuthoredAdmission

structure State where
  seq : Nat
  processed : Nat
  authorMatches : Bool
  edits : Nat
  deriving DecidableEq

inductive Verdict where
  | accepted | refused | duplicate
  deriving DecidableEq

def cost (s : State) : Nat := if s.authorMatches then 1 else 2

def admitted (limit : Nat) (s : State) (logicalValid authorReady : Bool) : Bool :=
  logicalValid && (s.authorMatches || authorReady) && decide (s.seq + cost s ≤ limit)

def submit (limit ticket : Nat) (logicalValid authorReady : Bool) (s : State) : State × Verdict :=
  if ticket ≤ s.processed then (s,.duplicate) else
  if admitted limit s logicalValid authorReady then
    ({seq := s.seq + cost s, processed := ticket, authorMatches := true, edits := s.edits + 1},.accepted)
  else ({s with processed := ticket},.refused)

theorem a_new_author_requires_two_positions (s : State) (unknown : s.authorMatches = false) :
    cost s = 2 := by simp [cost, unknown]

theorem a_matching_author_requires_one_position (s : State) (known : s.authorMatches = true) :
    cost s = 1 := by simp [cost, known]

theorem every_logical_refusal_preserves_author_content_and_log (limit ticket : Nat) (s : State)
    (authorReady : Bool) :
    (submit limit ticket false authorReady s).1.seq = s.seq ∧
    (submit limit ticket false authorReady s).1.authorMatches = s.authorMatches ∧
    (submit limit ticket false authorReady s).1.edits = s.edits := by
  unfold submit
  split <;> simp [admitted]

theorem failed_author_preparation_refuses_the_whole_edit (limit ticket : Nat) (s : State)
    (fresh : s.processed < ticket) (unknown : s.authorMatches = false) :
    submit limit ticket true false s = ({s with processed := ticket},.refused) := by
  simp [submit, admitted, Nat.not_le.mpr fresh, unknown]

theorem insufficient_headroom_never_publishes_a_lone_author (limit ticket : Nat) (s : State)
    (valid ready : Bool) (full : limit < s.seq + cost s) :
    (submit limit ticket valid ready s).1.seq = s.seq ∧
    (submit limit ticket valid ready s).1.authorMatches = s.authorMatches ∧
    (submit limit ticket valid ready s).1.edits = s.edits := by
  unfold submit
  split <;> simp [admitted, Nat.not_le.mpr full]

theorem new_refused_ticket_is_durable (limit ticket : Nat) (s : State) (valid ready : Bool)
    (fresh : s.processed < ticket) (refused : admitted limit s valid ready = false) :
    (submit limit ticket valid ready s).1.processed = ticket := by
  simp [submit, Nat.not_le.mpr fresh, refused]

theorem duplicate_tickets_cannot_recheck_or_register_authors (limit ticket : Nat) (s : State)
    (valid ready : Bool) (old : ticket ≤ s.processed) :
    submit limit ticket valid ready s = (s,.duplicate) := by simp [submit, old]

theorem changed_payload_cannot_revive_a_refused_ticket (limit ticket : Nat) (s : State)
    (valid ready corrected prepared : Bool) (fresh : s.processed < ticket)
    (refused : admitted limit s valid ready = false) :
    submit limit ticket corrected prepared (submit limit ticket valid ready s).1 =
      ((submit limit ticket valid ready s).1,.duplicate) := by
  apply duplicate_tickets_cannot_recheck_or_register_authors
  rw [new_refused_ticket_is_durable limit ticket s valid ready fresh refused]
  exact Nat.le_refl ticket

theorem successful_edits_publish_exactly_the_required_positions (limit ticket : Nat) (s : State)
    (valid ready : Bool) (fresh : s.processed < ticket)
    (accepted : admitted limit s valid ready = true) :
    submit limit ticket valid ready s =
      ({seq := s.seq + cost s, processed := ticket, authorMatches := true, edits := s.edits + 1},.accepted) := by
  simp [submit, Nat.not_le.mpr fresh, accepted]

theorem an_exact_fit_can_register_an_author_and_commit (s : State) (ticket : Nat)
    (fresh : s.processed < ticket) (unknown : s.authorMatches = false) :
    (submit (s.seq + 2) ticket true true s).2 = .accepted := by
  simp [submit, admitted, cost, unknown, Nat.not_le.mpr fresh]

theorem the_final_position_is_available_to_a_matching_author (s : State) (ticket : Nat)
    (fresh : s.processed < ticket) (known : s.authorMatches = true) :
    (submit (s.seq + 1) ticket true false s).2 = .accepted := by
  simp [submit, admitted, cost, known, Nat.not_le.mpr fresh]

theorem the_final_position_cannot_be_consumed_by_an_author_alone (s : State) (ticket : Nat)
    (fresh : s.processed < ticket) (unknown : s.authorMatches = false) :
    submit (s.seq + 1) ticket true true s = ({s with processed := ticket},.refused) := by
  simp [submit, admitted, cost, unknown, Nat.not_le.mpr fresh]

theorem submissions_preserve_log_bounds (limit ticket : Nat) (s : State)
    (valid ready : Bool) (bounded : s.seq ≤ limit) :
    (submit limit ticket valid ready s).1.seq ≤ limit := by
  unfold submit
  split
  · exact bounded
  · split
    · rename_i accepted
      simp [admitted] at accepted
      exact accepted.2
    · exact bounded

theorem processed_tickets_never_rewind (limit ticket : Nat) (s : State) (valid ready : Bool) :
    s.processed ≤ (submit limit ticket valid ready s).1.processed := by
  unfold submit
  split
  · exact Nat.le_refl _
  · rename_i fresh
    split <;> exact Nat.le_of_lt (Nat.lt_of_not_ge fresh)

def run (limit : Nat) (s : State) : List (Nat × Bool × Bool) → State
  | [] => s
  | (ticket,valid,ready) :: rest => run limit (submit limit ticket valid ready s).1 rest

theorem arbitrary_stateful_histories_remain_bounded (limit : Nat) (s : State)
    (steps : List (Nat × Bool × Bool)) (bounded : s.seq ≤ limit) :
    (run limit s steps).seq ≤ limit := by
  induction steps generalizing s with
  | nil => exact bounded
  | cons step rest ih =>
    apply ih
    exact submissions_preserve_log_bounds _ _ _ _ _ bounded

-- A checkpoint restores the exact healthy state, including processed refusals.
def restore (s : State) : State := s

theorem restore_never_revives_a_refused_submission (limit ticket : Nat) (s : State)
    (valid ready : Bool) (fresh : s.processed < ticket)
    (refused : admitted limit s valid ready = false) :
    submit limit ticket true true (restore (submit limit ticket valid ready s).1) =
      ((submit limit ticket valid ready s).1,.duplicate) := by
  exact changed_payload_cannot_revive_a_refused_ticket _ _ _ _ _ _ _ fresh refused

end JadeCollab.AuthoredAdmission
