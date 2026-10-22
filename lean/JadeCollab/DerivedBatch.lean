import Std
import JadeCollab.Journal

/-! Submission planning on independent model/file copies, followed by one
publication boundary. Fork independence and native editor behavior are abstract
assumptions checked by Rust fault injection and real Office restart tests.
-/
namespace JadeCollab.DerivedBatch

structure State (D F : Type) where
  document : D
  file : F
  seq : Nat
  processed : Nat
  deriving DecidableEq

structure Plan (D F Op : Type) where
  document : D
  file : F
  cause : Op
  derived : List Op

def publish {D F Op : Type} (limit : Nat) (state : State D F) (ticket : Nat)
    (plan : Option (Plan D F Op)) : State D F :=
  if ticket ≤ state.processed then state
  else
    let refused := { state with processed := ticket }
    match plan with
    | none => refused
    | some planned =>
      if state.seq + 1 + planned.derived.length ≤ limit then
        { document := planned.document, file := planned.file,
          seq := state.seq + 1 + planned.derived.length, processed := ticket }
      else refused

theorem failed_plan_preserves_document_file_and_log {D F Op : Type}
    (limit : Nat) (state : State D F) (ticket : Nat) :
    (publish (Op := Op) limit state ticket none).document = state.document ∧
    (publish (Op := Op) limit state ticket none).file = state.file ∧
    (publish (Op := Op) limit state ticket none).seq = state.seq := by
  unfold publish
  split <;> simp_all

theorem insufficient_capacity_preserves_document_file_and_log {D F Op : Type}
    (limit : Nat) (state : State D F) (ticket : Nat) (planned : Plan D F Op)
    (full : limit < state.seq + 1 + planned.derived.length) :
    (publish limit state ticket (some planned)).document = state.document ∧
    (publish limit state ticket (some planned)).file = state.file ∧
    (publish limit state ticket (some planned)).seq = state.seq := by
  by_cases answered : ticket ≤ state.processed <;>
    simp [publish, answered, Nat.not_le.mpr full]

theorem successful_plan_publishes_the_entire_suffix {D F Op : Type}
    (limit : Nat) (state : State D F) (ticket : Nat) (planned : Plan D F Op)
    (fresh : state.processed < ticket)
    (fits : state.seq + 1 + planned.derived.length ≤ limit) :
    publish limit state ticket (some planned) =
      { document := planned.document, file := planned.file,
        seq := state.seq + 1 + planned.derived.length, processed := ticket } := by
  simp [publish, Nat.not_le.mpr fresh, fits]

theorem exact_capacity_is_accepted {D F Op : Type}
    (state : State D F) (ticket : Nat) (planned : Plan D F Op)
    (fresh : state.processed < ticket) :
    (publish (state.seq + 1 + planned.derived.length) state ticket (some planned)).seq =
      state.seq + 1 + planned.derived.length := by
  simp [publish, Nat.not_le.mpr fresh]

theorem publication_stays_within_log_bound {D F Op : Type}
    (limit : Nat) (state : State D F) (ticket : Nat) (planned : Option (Plan D F Op))
    (bounded : state.seq ≤ limit) : (publish limit state ticket planned).seq ≤ limit := by
  cases planned with
  | none => by_cases answered : ticket ≤ state.processed <;> simp [publish, answered, bounded]
  | some planned =>
    by_cases answered : ticket ≤ state.processed <;>
      by_cases fits : state.seq + 1 + planned.derived.length ≤ limit <;>
        simp [publish, answered, fits, bounded]

theorem full_log_cannot_publish_even_an_empty_suffix {D F Op : Type}
    (state : State D F) (ticket : Nat) (planned : Plan D F Op) :
    (publish state.seq state ticket (some planned)).seq = state.seq := by
  have full : ¬ state.seq + 1 + planned.derived.length ≤ state.seq := by omega
  by_cases answered : ticket ≤ state.processed <;> simp [publish, answered, full]

theorem refused_ticket_is_still_processed {D F Op : Type}
    (limit : Nat) (state : State D F) (ticket : Nat) (planned : Option (Plan D F Op))
    (fresh : state.processed < ticket) :
    (publish limit state ticket planned).processed = ticket := by
  cases planned <;> simp [publish, Nat.not_le.mpr fresh] <;> split <;> simp_all

theorem a_retry_never_republishes_an_answered_ticket {D F Op : Type}
    (limit : Nat) (state : State D F) (ticket : Nat) (planned : Option (Plan D F Op))
    (answered : ticket ≤ state.processed) : publish limit state ticket planned = state := by
  simp [publish, answered]

theorem failed_plan_cannot_be_revived_by_retry {D F Op : Type}
    (limit : Nat) (state : State D F) (ticket : Nat) (planned : Option (Plan D F Op))
    (fresh : state.processed < ticket) :
    publish limit (publish (Op := Op) limit state ticket none) ticket planned =
      publish (Op := Op) limit state ticket none := by
  apply a_retry_never_republishes_an_answered_ticket
  simp [refused_ticket_is_still_processed limit state ticket none fresh]

def allocate (limit time count : Nat) : Option Nat :=
  if time + count ≤ limit then some (time + count) else none

theorem no_identifiers_needed_at_the_limit (limit : Nat) : allocate limit limit 0 = some limit := by
  simp [allocate]

theorem exact_identifier_capacity_is_accepted (time count : Nat) :
    allocate (time + count) time count = some (time + count) := by simp [allocate]

theorem exhausted_identifiers_are_refused (limit time count : Nat)
    (full : limit < time + count) : allocate limit time count = none := by
  simp [allocate, Nat.not_le.mpr full]

theorem allocated_identifiers_stay_bounded (limit time count last : Nat)
    (accepted : allocate limit time count = some last) : time ≤ last ∧ last ≤ limit := by
  unfold allocate at accepted
  split at accepted <;> simp_all <;> omega

theorem persisted_batch_contains_cause_and_every_child {S Op : Type}
    (step : S → Op → S) (store : JadeCollab.Journal.Store S Op)
    (cause : Op) (derived : List Op) :
    JadeCollab.Journal.recover step (JadeCollab.Journal.appendBatch store (cause :: derived)) =
      derived.foldl step (step (JadeCollab.Journal.recover step store) cause) := by
  rw [JadeCollab.Journal.recover_appendBatch]
  rfl

end JadeCollab.DerivedBatch
