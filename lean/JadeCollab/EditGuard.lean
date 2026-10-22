/-! Atomic optimistic edit guards. The SDK locks the replica for version
validation, planning, allocation and queueing. These proofs specify that
single transition; Rust tests check actual locking, FIFO and payloads. -/
namespace JadeCollab.EditGuard

structure Version where
  owner : Nat
  log : Nat
  pending : Nat
  nextTicket : Nat
deriving DecidableEq

structure State (α : Type) where
  version : Version
  document : α
  clock : Nat
  outgoing : List Nat

def run (expected : Version) (state : State α) (action : State α → State α × β) :
    State α × Option β :=
  if expected = state.version then
    let (after, result) := action state
    (after, some result)
  else (state, none)

theorem stale_preserves_everything (expected : Version) (state : State α)
    (action : State α → State α × β) (h : expected ≠ state.version) :
    run expected state action = (state, none) := by
  simp [run, h]

theorem matching_runs_action (state : State α) (action : State α → State α × β) :
    run state.version state action = ((action state).1, some (action state).2) := by
  simp [run]

/-- A changed version makes a competing action using the old snapshot fail,
even if the document text happens to be equal. -/
theorem competing_old_guard_fails (before after : State α)
    (action : State α → State α × β) (h : before.version ≠ after.version) :
    run before.version after action = (after, none) := stale_preserves_everything _ _ _ h

/-- A rejected optimistic edit can restore the same text/log/pending count,
but it cannot restore the old next-ticket number. -/
theorem advanced_ticket_prevents_aba (before after : Version)
    (h : before.nextTicket < after.nextTicket) : before ≠ after := by
  intro equal
  have same := congrArg Version.nextTicket equal
  omega

theorem foreign_owner_is_stale (before after : Version)
    (h : before.owner ≠ after.owner) : before ≠ after := by
  intro equal
  exact h (congrArg Version.owner equal)

def next (ticket cap : Nat) : Option Nat :=
  if ticket < cap then some (ticket + 1) else none

theorem accepted_ticket_advances (ticket cap cursor : Nat) (h : next ticket cap = some cursor) :
    ticket < cursor ∧ cursor ≤ cap := by
  unfold next at h
  split at h <;> simp_all <;> omega

theorem exhausted_ticket_is_refused (cap : Nat) : next cap cap = none := by
  simp [next]

inductive PlanError (ε : Type) where
  | stale
  | refused (error : ε)
deriving DecidableEq

/-- Plan on a private state, publishing it only after local success. -/
def tryRun (expected : Version) (state : State α)
    (action : State α → State α × Except ε β) : State α × Except (PlanError ε) β :=
  if expected = state.version then
    match action state with
    | (after, .ok value) => (after, .ok value)
    | (_, .error error) => (state, .error (.refused error))
  else (state, .error .stale)

theorem stale_plan_preserves_everything (expected : Version) (state : State α)
    (action : State α → State α × Except ε β) (h : expected ≠ state.version) :
    tryRun expected state action = (state, .error .stale) := by
  simp [tryRun, h]

theorem planning_failure_rolls_back (state after : State α)
    (action : State α → State α × Except ε β) (error : ε)
    (h : action state = (after, .error error)) :
    tryRun state.version state action = (state, .error (.refused error)) := by
  simp [tryRun, h]

theorem planning_success_commits (state after : State α)
    (action : State α → State α × Except ε β) (value : β)
    (h : action state = (after, .ok value)) :
    tryRun state.version state action = (after, .ok value) := by
  simp [tryRun, h]

theorem failed_plan_allows_retry (state after : State α)
    (action retry : State α → State α × Except ε β) (error : ε)
    (h : action state = (after, .error error)) :
    tryRun state.version (tryRun state.version state action).1 retry =
      tryRun state.version state retry := by
  rw [planning_failure_rolls_back state after action error h]

/-- The outgoing queue publishes one plan as a contiguous append. -/
def publish (state : State α) (messages : List Nat) : State α :=
  { state with outgoing := state.outgoing ++ messages }

theorem publication_order (state : State α) (first second : List Nat) :
    (publish (publish state first) second).outgoing =
      state.outgoing ++ (first ++ second) := by
  simp only [publish, List.append_assoc]

theorem publication_keeps_document_clock (state : State α) (messages : List Nat) :
    (publish state messages).document = state.document ∧
    (publish state messages).clock = state.clock ∧
    (publish state messages).version = state.version := by
  simp [publish]

end JadeCollab.EditGuard
