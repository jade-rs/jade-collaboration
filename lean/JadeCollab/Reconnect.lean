/-! Recoverable resumption: validate the processed-ticket watermark, stage
every catch-up operation, and retain the source on refusal. This models the
transaction boundary, not the socket protocol or document-specific guards. -/
namespace JadeCollab.Reconnect

def validWatermark (answered next processed : Nat) : Prop :=
  answered ≤ processed ∧ processed < next

instance (answered next processed : Nat) : Decidable (validWatermark answered next processed) := by
  unfold validWatermark; infer_instance

theorem accepted_watermark_is_issued (answered next processed limit : Nat)
    (h : validWatermark answered next processed) (bound : next ≤ limit) :
    answered ≤ processed ∧ processed + 1 ≤ next ∧ processed + 1 ≤ limit := by
  unfold validWatermark at h; omega

theorem forgotten_answers_are_refused (answered next processed : Nat)
    (h : processed < answered) : ¬validWatermark answered next processed := by
  unfold validWatermark; omega

def remaining (processed : Nat) (pending : List Nat) : List Nat :=
  pending.filter (processed < ·)

theorem unprocessed_ticket_survives (processed ticket : Nat) (pending : List Nat) :
    ticket ∈ remaining processed pending ↔ ticket ∈ pending ∧ processed < ticket := by
  simp [remaining]

def replay (step : S → Op → Option S) : S → List Op → Option S
  | state, [] => some state
  | state, op :: rest => (step state op).bind (fun next => replay step next rest)

theorem replay_append (step : S → Op → Option S) (state : S) (a b : List Op) :
    replay step state (a ++ b) = (replay step state a).bind (fun mid => replay step mid b) := by
  induction a generalizing state with
  | nil => rfl
  | cons op rest ih =>
    simp only [List.cons_append, replay]
    cases step state op with
    | none => rfl
    | some next => exact ih next

def apply (step : S → Op → Option S) (state : S) (batch : List Op) : S :=
  (replay step state batch).getD state

theorem failure_after_a_valid_prefix_is_atomic (step : S → Op → Option S)
    (state mid : S) (before tail : List Op) (bad : Op)
    (good : replay step state before = some mid) (failure : step mid bad = none) :
    apply step state (before ++ bad :: tail) = state := by
  simp [apply, replay_append, good, replay, failure]

theorem staging_preserves_source (step : S → Op → Option S) (source : S)
    (a b : List Op) (h : replay step source a = none) :
    apply step source (a ++ b) = source := by
  simp [apply, replay_append, h]

/-! One joined replica's server watermark. Access refusal and native guard
refusal both consume the ticket, while duplicates cannot change the document
even if its holder later gains edit permission. Durability-before-reply is
checked by the journal/actor tests, not modeled as an IO action here. -/
structure TicketState (S : Type) where
  document : S
  processed : Nat

def process (state : TicketState S) (ticket : Nat) (editable : Bool)
    (operation : S → Option S) : TicketState S :=
  if ticket ≤ state.processed then state
  else { document := if editable then (operation state.document).getD state.document else state.document
         processed := ticket }

theorem refusal_never_changes_document (state : TicketState S) (ticket : Nat)
    (operation : S → Option S) :
    (process state ticket false operation).document = state.document := by
  unfold process
  split <;> rfl

theorem fresh_refusal_consumes_ticket (state : TicketState S) (ticket : Nat)
    (operation : S → Option S) (fresh : state.processed < ticket) :
    (process state ticket false operation).processed = ticket := by
  simp [process, Nat.not_le.mpr fresh]

theorem refusal_cannot_commit_after_promotion (state : TicketState S) (ticket : Nat)
    (operation : S → Option S) :
    process (process state ticket false operation) ticket true operation =
      process state ticket false operation := by
  by_cases old : ticket ≤ state.processed
  · simp [process, old]
  · simp [process, old]

theorem processed_watermark_never_regresses (state : TicketState S) (ticket : Nat)
    (editable : Bool) (operation : S → Option S) :
    state.processed ≤ (process state ticket editable operation).processed := by
  unfold process
  split <;> simp_all <;> omega

end JadeCollab.Reconnect
