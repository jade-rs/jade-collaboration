import Std

/-! SDK writer policy. `cursor` is the next unsent ticket; inspection barriers
are exclusive ticket boundaries captured while holding the replica lock.
The model abstracts socket encoding, IO and the single in-flight frame.
Rust stateful and gated-sink tests check the implementation against this policy.
-/
namespace JadeCollab.ClientPump

inductive Action where
  | edit (ticket : Nat)
  | inspect
  | presence
  deriving DecidableEq

def ordinary (cursor issued : Nat) (barrier : Option Nat) : Option Action :=
  if barrier.any (· ≤ cursor) then some .inspect
  else if cursor < issued then some (.edit cursor) else none

def choose (cursor issued : Nat) (barrier : Option Nat)
    (hasPresence lastPresence failed : Bool) : Option Action :=
  if failed then none
  else if hasPresence && (!lastPresence || (ordinary cursor issued barrier).isNone)
    then some .presence
  else ordinary cursor issued barrier

theorem edit_is_fifo (cursor issued ticket : Nat) (barrier : Option Nat)
    (h : ordinary cursor issued barrier = some (.edit ticket)) :
    ticket = cursor ∧ cursor < issued ∧ barrier.all (cursor < ·) = true := by
  cases barrier with
  | none => simp [ordinary] at h ⊢; omega
  | some boundary =>
    by_cases hb : boundary ≤ cursor <;> by_cases hi : cursor < issued <;>
      simp_all [ordinary] <;> omega

theorem inspection_waits_for_earlier_tickets (cursor issued boundary : Nat)
    (h : ordinary cursor issued (some boundary) = some .inspect) : boundary ≤ cursor := by
  simpa [ordinary] using h

theorem inspection_precedes_later_edits (cursor issued boundary : Nat)
    (h : boundary ≤ cursor) : ordinary cursor issued (some boundary) = some .inspect := by
  simp [ordinary, h]

theorem chosen_edit_obeys_fifo_and_inspection_barrier (cursor issued ticket : Nat)
    (barrier : Option Nat) (hasPresence lastPresence failed : Bool)
    (h : choose cursor issued barrier hasPresence lastPresence failed = some (.edit ticket)) :
    ticket = cursor ∧ cursor < issued ∧ barrier.all (cursor < ·) = true := by
  unfold choose at h
  split at h
  · simp at h
  · split at h
    · simp at h
    · exact edit_is_fifo cursor issued ticket barrier h

theorem chosen_inspection_waits_for_earlier_tickets (cursor issued boundary : Nat)
    (hasPresence lastPresence failed : Bool)
    (h : choose cursor issued (some boundary) hasPresence lastPresence failed = some .inspect) :
    boundary ≤ cursor := by
  unfold choose at h
  split at h
  · simp at h
  · split at h
    · simp at h
    · exact inspection_waits_for_earlier_tickets cursor issued boundary h

theorem issued_ticket_has_a_bounded_successor (ticket limit : Nat) (h : ticket < limit) :
    ticket + 1 ≤ limit := by omega

theorem presence_does_not_starve_edits_or_inspections (cursor issued : Nat)
    (barrier : Option Nat) (action : Action)
    (ready : ordinary cursor issued barrier = some action) :
    choose cursor issued barrier true true false = some action := by
  simp [choose, ready]

theorem failed_connection_sends_nothing (cursor issued : Nat) (barrier : Option Nat)
    (hasPresence lastPresence : Bool) :
    choose cursor issued barrier hasPresence lastPresence true = none := by simp [choose]

structure State where
  pending : List Nat
  cursor : Nat
  barriers : List Nat
  presence : Option Nat
  failed : Bool
  deriving DecidableEq

def share (state : State) (value : Nat) : State := { state with presence := some value }
def fail (state : State) : State := { state with failed := true }
def cancel (state : State) (boundary : Nat) : State :=
  { state with barriers := state.barriers.filter (· != boundary) }

-- Sending advances only the transport cursor. Verdict processing, separately,
-- removes a FIFO prefix from the replica; a failed send must retain that source.
def sent (state : State) : State := { state with cursor := state.cursor + 1 }

theorem presence_coalesces_to_latest (state : State) (old latest : Nat) :
    share (share state old) latest = share state latest := rfl

theorem presence_preserves_document_order (state : State) (value : Nat) :
    (share state value).pending = state.pending ∧
    (share state value).barriers = state.barriers ∧
    (share state value).cursor = state.cursor := by simp [share]

theorem cancelling_inspection_preserves_edits (state : State) (boundary : Nat) :
    (cancel state boundary).pending = state.pending ∧
    (cancel state boundary).cursor = state.cursor := by simp [cancel]

theorem sent_edit_keeps_the_retry_source (state : State) :
    (sent state).pending = state.pending ∧
    (sent state).cursor = state.cursor + 1 := by simp [sent]

theorem failed_send_keeps_the_retry_source (state : State) :
    (fail state).pending = state.pending ∧
    (fail state).cursor = state.cursor ∧ (fail state).failed = true := by simp [fail]

theorem repeated_failure_preserves_the_retry_source (state : State) :
    fail (fail state) = fail state := rfl

end JadeCollab.ClientPump
