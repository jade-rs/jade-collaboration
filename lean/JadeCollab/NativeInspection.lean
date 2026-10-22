import JadeCollab.EditGuard

/-! An authoritative read is one actor transition: model, native formatting
and log position are captured together. A request retains its connection's
ticket so local edits queued after it invalidate the returned edit guard.
Native OOXML resolution and real request lifetimes are tested in Rust. -/
namespace JadeCollab.NativeInspection

structure Authority (α β : Type) where
  document : α
  native : β
  log : Nat

structure Request where
  owner : Nat
  nextTicket : Nat

structure Snapshot (α γ : Type) where
  document : α
  formatting : γ
  version : EditGuard.Version

def capture (request : Request) (state : Authority α β) (project : α → β → γ) : Snapshot α γ :=
  { document := state.document
    formatting := project state.document state.native
    version := ⟨request.owner, state.log, 0, request.nextTicket⟩ }

def read (request : Request) (state : Authority α β) (project : α → β → γ) :=
  (state, capture request state project)

theorem read_preserves_authority (request : Request) (state : Authority α β)
    (project : α → β → γ) : (read request state project).1 = state := rfl

theorem coherent_capture (request : Request) (state : Authority α β) (project : α → β → γ) :
    (capture request state project).document = state.document ∧
    (capture request state project).formatting = project state.document state.native ∧
    (capture request state project).version.log = state.log ∧
    (capture request state project).version.pending = 0 := ⟨rfl, rfl, rfl, rfl⟩

theorem later_local_edit_refuses_guard (request : Request) (state : Authority α β)
    (project : α → β → γ) (live : EditGuard.State α)
    (action : EditGuard.State α → EditGuard.State α × δ)
    (h : request.nextTicket < live.version.nextTicket) :
    EditGuard.run (capture request state project).version live action = (live, none) := by
  apply EditGuard.stale_preserves_everything
  apply EditGuard.advanced_ticket_prevents_aba
  exact h

theorem later_log_refuses_guard (request : Request) (state : Authority α β)
    (project : α → β → γ) (live : EditGuard.State α)
    (action : EditGuard.State α → EditGuard.State α × δ)
    (h : state.log < live.version.log) :
    EditGuard.run (capture request state project).version live action = (live, none) := by
  apply EditGuard.stale_preserves_everything
  intro equal
  have same := congrArg EditGuard.Version.log equal
  change state.log = live.version.log at same
  omega

end JadeCollab.NativeInspection
