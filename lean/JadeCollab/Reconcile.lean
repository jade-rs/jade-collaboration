/-!
# Soundness of the client fast path

design.md §2: a client keeps the server-confirmed state and a queue of pending
local operations; its optimistic view is `confirmed` with `pending` replayed on
top. A remote *sequenced* operation forces `view = confirmed + pending` to be
rebuilt, but a remote *convergent* operation is applied straight to the view.
This module shows that shortcut is exact. Backs `Replica` in
`jade-collab-sync`.
-/

namespace JadeCollab.Reconcile

variable {S Op : Type} (step : S → Op → S)

/-- The client's optimistic view. -/
def view (confirmed : S) (pending : List Op) : S :=
  pending.foldl step confirmed

/-- **Fast path soundness** (`fast_path_sound`, design.md §2 "Typing never
rebases"): if the remote operation `c` commutes with every operation, applying
it to the optimistic view equals rebuilding the view from the new confirmed
state. -/
theorem fast_path_sound (c : Op) (hc : ∀ s o, step (step s c) o = step (step s o) c) :
    ∀ (pending : List Op) (confirmed : S),
      step (pending.foldl step confirmed) c = pending.foldl step (step confirmed c) := by
  intro pending
  induction pending with
  | nil => intro confirmed; rfl
  | cons o ps ih =>
    intro confirmed
    simp only [List.foldl_cons]
    rw [ih, hc]

/-- The fast path for a batch of remote convergent operations. -/
theorem fast_path_sound_many (cs : List Op)
    (hc : ∀ c ∈ cs, ∀ s o, step (step s c) o = step (step s o) c) :
    ∀ (pending : List Op) (confirmed : S),
      cs.foldl step (view step confirmed pending) = view step (cs.foldl step confirmed) pending := by
  induction cs with
  | nil => intro pending confirmed; rfl
  | cons c cs ih =>
    intro pending confirmed
    simp only [List.foldl_cons]
    unfold view at ih ⊢
    rw [fast_path_sound step c (hc c List.mem_cons_self),
      ih (fun c' hc' => hc c' (List.mem_cons_of_mem _ hc'))]

/-- With nothing pending, the view is the confirmed state. -/
theorem quiescent_view_eq_server (confirmed : S) : view step confirmed [] = confirmed :=
  rfl

/-- **Quiescence**: once a client has folded the server's whole log into its
confirmed state — in any number of catch-up chunks `L₁`, `L₂` — and has no
pending operations, its view is exactly the server's state for `L₁ ++ L₂`. -/
theorem quiescent_converges (s₀ : S) (L₁ L₂ : List Op) :
    view step (L₂.foldl step (L₁.foldl step s₀)) [] = (L₁ ++ L₂).foldl step s₀ := by
  rw [quiescent_view_eq_server, List.foldl_append]

end JadeCollab.Reconcile
