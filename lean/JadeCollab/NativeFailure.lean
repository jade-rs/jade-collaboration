import Std

/-! Native errors may leave arbitrary partial model/file writes. Only the
healthy committed prefix is durable. Recovery below denotes loading that prefix
from an independent checkpoint plus journal, never exporting quarantined state.
Successful native application, fork isolation, and journal IO are assumptions;
Rust tests inject failures around real writes and compare stored bytes/retries. -/
namespace JadeCollab.NativeFailure

structure State (Op : Type) where
  history : List Op
  working : List Op
  native : List Op
  processed : Nat
  healthy : Bool
  deriving DecidableEq

inductive NativeResult (Op : Type) where
  | success
  | stagedFailure
  | liveFailure (working native : List Op)

inductive Verdict where
  | acknowledged
  | refused
  | duplicate
  | unknown
  deriving DecidableEq

def fail (s : State Op) (working native : List Op) : State Op :=
  {s with working, native, healthy := false}

def submit (s : State Op) (ticket : Nat) (op : Op) (result : NativeResult Op) :
    State Op × Verdict :=
  if s.healthy = false then (s, .unknown) else
  if ticket ≤ s.processed then (s, .duplicate) else
  match result with
  | .success =>
    ({s with history := s.history ++ [op], working := s.working ++ [op],
             native := s.native ++ [op], processed := ticket}, .acknowledged)
  | .stagedFailure => ({s with processed := ticket}, .refused)
  | .liveFailure working native => (fail s working native, .unknown)

def capture (s : State Op) : Option (List Op × List Op) :=
  if s.healthy then some (s.working, s.native) else none

def recover (s : State Op) : State Op :=
  {s with working := s.history, native := s.history, healthy := true}

def run (s : State Op) (commands : List (Nat × Op × NativeResult Op)) : State Op :=
  commands.foldl (fun state (ticket, op, result) => (submit state ticket op result).1) s

theorem live_error_has_no_verdict (s : State Op) (ticket : Nat) (op : Op)
    (working native : List Op) (healthy : s.healthy = true) (fresh : s.processed < ticket) :
    (submit s ticket op (.liveFailure working native)).2 = .unknown := by
  simp [submit, healthy, Nat.not_le.mpr fresh]

theorem live_error_preserves_committed_prefix_and_ticket (s : State Op) (ticket : Nat)
    (op : Op) (working native : List Op) :
    (submit s ticket op (.liveFailure working native)).1.history = s.history ∧
    (submit s ticket op (.liveFailure working native)).1.processed = s.processed := by
  unfold submit
  split <;> (try split) <;> simp [fail]

theorem partial_writes_are_quarantined (s : State Op) (ticket : Nat) (op : Op)
    (working native : List Op) (healthy : s.healthy = true) (fresh : s.processed < ticket) :
    capture (submit s ticket op (.liveFailure working native)).1 = none := by
  simp [submit, healthy, Nat.not_le.mpr fresh, capture, fail]

theorem failed_instance_cannot_serve_snapshot (s : State Op) (failed : s.healthy = false) :
    capture s = none := by simp [capture, failed]

theorem failed_instance_refuses_every_further_transition (s : State Op) (ticket : Nat)
    (op : Op) (result : NativeResult Op) (failed : s.healthy = false) :
    submit s ticket op result = (s, .unknown) := by simp [submit, failed]

theorem arbitrary_history_cannot_revive_failed_instance (s : State Op)
    (commands : List (Nat × Op × NativeResult Op)) (failed : s.healthy = false) :
    run s commands = s := by
  induction commands with
  | nil => rfl
  | cons command tail ih =>
    rcases command with ⟨ticket, op, result⟩
    simpa [run, List.foldl_cons, submit, failed] using ih

theorem staged_error_is_an_atomic_refusal (s : State Op) (ticket : Nat) (op : Op)
    (healthy : s.healthy = true) (fresh : s.processed < ticket) :
    submit s ticket op .stagedFailure = ({s with processed := ticket}, .refused) := by
  simp [submit, healthy, Nat.not_le.mpr fresh]

theorem refused_stage_preserves_file_model_and_log (s : State Op) (ticket : Nat) (op : Op) :
    (submit s ticket op .stagedFailure).1.working = s.working ∧
    (submit s ticket op .stagedFailure).1.native = s.native ∧
    (submit s ticket op .stagedFailure).1.history = s.history := by
  unfold submit
  split <;> (try split) <;> simp

theorem staged_error_cannot_be_revived_by_changed_payload (s : State Op) (ticket : Nat)
    (op changed : Op) (result : NativeResult Op) (healthy : s.healthy = true)
    (fresh : s.processed < ticket) :
    (submit (submit s ticket op .stagedFailure).1 ticket changed result).2 = .duplicate := by
  simp [submit, healthy, Nat.not_le.mpr fresh]

theorem recovery_discards_every_partial_write (s : State Op) (working native : List Op) :
    recover (fail s working native) = recover s := by rfl

theorem recovered_model_and_native_agree_with_durable_history (s : State Op) :
    (recover s).working = s.history ∧ (recover s).native = s.history ∧
    (recover s).healthy = true ∧ (recover s).processed = s.processed := by simp [recover]

theorem recovery_is_idempotent (s : State Op) : recover (recover s) = recover s := by rfl

theorem failed_ticket_can_be_retried_after_recovery (s : State Op) (ticket : Nat) (op : Op)
    (working native : List Op) (fresh : s.processed < ticket) :
    submit (recover (fail s working native)) ticket op .success =
      ({history := s.history ++ [op], working := s.history ++ [op],
        native := s.history ++ [op], processed := ticket, healthy := true}, .acknowledged) := by
  simp [submit, recover, fail, Nat.not_le.mpr fresh]

theorem successful_retry_publishes_exactly_one_operation (s : State Op) (ticket : Nat) (op : Op)
    (fresh : s.processed < ticket) :
    (submit (recover s) ticket op .success).1.history.length = s.history.length + 1 := by
  simp [submit, recover, Nat.not_le.mpr fresh]

theorem second_retry_never_repeats_the_native_write (s : State Op) (ticket : Nat)
    (op changed : Op) (result : NativeResult Op) (fresh : s.processed < ticket) :
    submit (submit (recover s) ticket op .success).1 ticket changed result =
      ((submit (recover s) ticket op .success).1, .duplicate) := by
  simp [submit, recover, Nat.not_le.mpr fresh]

theorem healthy_success_preserves_native_model_agreement (s : State Op) (ticket : Nat) (op : Op)
    (model : s.working = s.history) (file : s.native = s.history) :
    (submit s ticket op .success).1.working = (submit s ticket op .success).1.history ∧
    (submit s ticket op .success).1.native = (submit s ticket op .success).1.history := by
  unfold submit
  split <;> (try split) <;> simp [model, file]

theorem origin_failure_preserves_log_and_all_client_tickets (s : State Op)
    (working native : List Op) :
    (fail s working native).history = s.history ∧
    (fail s working native).processed = s.processed ∧
    capture (fail s working native) = none := by simp [fail, capture]

end JadeCollab.NativeFailure
