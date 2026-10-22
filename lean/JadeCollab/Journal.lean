/-!
# Recovery at submission boundaries

`DocumentStore` commits a submission and its derived operations with one
newline-terminated frame. Only complete frames are replayed; a torn final
frame is discarded. Checkpoints are taken after the whole frame is written.

This is a model of those boundaries, not a proof of filesystem durability.
The Rust stateful and fault-injection tests exercise the implementation.
-/

namespace JadeCollab.Journal

variable {S Op : Type}

structure Store (S Op : Type) where
  snapshot : S
  complete : List (List Op)
  unfinished : List Op

/-- Recovery ignores the unterminated final frame. -/
def recover (step : S → Op → S) (store : Store S Op) : S :=
  store.complete.foldl (fun state batch => batch.foldl step state) store.snapshot

def appendBatch (store : Store S Op) (batch : List Op) : Store S Op :=
  { store with complete := store.complete ++ [batch], unfinished := [] }

def interrupt (store : Store S Op) (fragment : List Op) : Store S Op :=
  { store with unfinished := fragment }

def checkpoint (step : S → Op → S) (store : Store S Op) : Store S Op :=
  { snapshot := recover step store, complete := [], unfinished := [] }

/-- The cause and every derived operation are recovered in order, once. -/
theorem recover_appendBatch (step : S → Op → S) (store : Store S Op)
    (batch : List Op) :
    recover step (appendBatch store batch) = batch.foldl step (recover step store) := by
  simp [recover, appendBatch, List.foldl_append]

/-- Every partial write, even one containing the whole cause, is invisible. -/
theorem recover_interrupt (step : S → Op → S) (store : Store S Op)
    (fragment : List Op) :
    recover step (interrupt store fragment) = recover step store := by
  rfl

/-- Moving complete frames into the snapshot preserves the recovered state. -/
theorem recover_checkpoint (step : S → Op → S) (store : Store S Op) :
    recover step (checkpoint step store) = recover step store := by
  rfl

/-- Checkpointing after the submission does not replay its derived suffix. -/
theorem checkpoint_batch (step : S → Op → S) (store : Store S Op)
    (cause : Op) (derived : List Op) :
    recover step (checkpoint step (appendBatch store (cause :: derived))) =
      derived.foldl step (step (recover step store) cause) := by
  rw [recover_checkpoint, recover_appendBatch]
  rfl

/-- Several frames recover exactly as their flattened operation log. -/
theorem recover_flatten (step : S → Op → S) (store : Store S Op) :
    recover step store = store.complete.flatten.foldl step store.snapshot := by
  have h : ∀ (batches : List (List Op)) (state : S),
      batches.foldl (fun s batch => batch.foldl step s) state =
        batches.flatten.foldl step state := by
    intro batches
    induction batches with
    | nil => intro state; rfl
    | cons batch batches ih =>
      intro state
      simp [List.foldl_cons, List.flatten_cons, List.foldl_append, ih]
  exact h store.complete store.snapshot

/-- The former ordering is unsafe even for ordinary additions: snapshotting
the derived effect and then journaling it again applies it twice. -/
theorem split_checkpoint_duplicates_derived :
    recover Nat.add (appendBatch (checkpoint Nat.add
      (appendBatch ⟨0, [], []⟩ [1, 10])) [10]) = 21 := by
  decide

end JadeCollab.Journal
