import Std

/-! Notes run maintenance preserves the paragraph's styled character stream and
opaque XML anchors. Parsing/namespaces, raw XML joins and native Office rendering
are Rust/native-PBT boundaries, not assumed to be proved by this model.
The count bound is for one eligible group checked before each successful
text-changing native prepend adding at most two runs. Rich/opaque barriers
remain separate. Publication/replay lemmas concern the native archive only;
the deterministic native transition is a Rust/native verification boundary. -/
namespace JadeCollab.NotesRuns

inductive Piece (Char Meta : Type) where
  | plain (space : Nat) (text : List Char)
  | opaque (raw : Meta)

inductive Token (Char Meta : Type) where
  | character (space : Nat) (value : Char)
  | anchor (raw : Meta)

def view : Piece Char Meta → List (Token Char Meta)
  | .plain space text => text.map (.character space)
  | .opaque raw => [.anchor raw]

def observe (pieces : List (Piece Char Meta)) : List (Token Char Meta) :=
  pieces.flatMap view

theorem joining_equivalent_plain_runs_preserves_characters_and_space
    (space : Nat) (a b : List Char) :
    view (Meta := Meta) (.plain space (a ++ b)) =
      view (.plain space a) ++ view (.plain space b) := by
  simp [view]

inductive Rewrite : List (Piece Char Meta) → List (Piece Char Meta) → Prop where
  | adjacent (pre post : List (Piece Char Meta)) (space : Nat) (a b : List Char) :
      Rewrite (pre ++ [.plain space a, .plain space b] ++ post)
        (pre ++ [.plain space (a ++ b)] ++ post)

theorem a_local_rewrite_preserves_the_complete_observable_stream
    {before after : List (Piece Char Meta)} (step : Rewrite before after) :
    observe before = observe after := by
  cases step with
  | adjacent pre post space a b => simp [observe, view, List.append_assoc]

def anchors (pieces : List (Piece Char Meta)) : List Meta :=
  (observe pieces).filterMap fun token => match token with
    | .anchor raw => some raw
    | .character _ _ => none

theorem opaque_payloads_keep_their_exact_order_and_values
    {before after : List (Piece Char Meta)} (step : Rewrite before after) :
    anchors before = anchors after := by
  simp only [anchors, a_local_rewrite_preserves_the_complete_observable_stream step]

inductive History : List (Piece Char Meta) → List (Piece Char Meta) → Prop where
  | same (pieces) : History pieces pieces
  | next {first middle last} : History first middle → Rewrite middle last → History first last

theorem arbitrarily_many_maintenance_steps_preserve_all_observations
    {before after : List (Piece Char Meta)} (history : History before after) :
    observe before = observe after := by
  induction history with
  | same => rfl
  | next _ step ih => exact ih.trans (a_local_rewrite_preserves_the_complete_observable_stream step)

theorem every_interpreter_of_the_preserved_stream_agrees
    {before after : List (Piece Char Meta)} (history : History before after)
    (interpret : List (Token Char Meta) → Result) :
    interpret (observe before) = interpret (observe after) :=
  congrArg interpret (arbitrarily_many_maintenance_steps_preserve_all_observations history)

def compactCount (runs : Nat) : Nat := if 16 ≤ runs then 1 else runs

theorem an_eligible_group_is_small_after_a_check (runs : Nat) : compactCount runs ≤ 15 := by
  unfold compactCount
  split <;> omega

def tick (runs added : Nat) : Nat := compactCount runs + added

theorem every_prepend_bounds_even_an_arbitrarily_large_restored_group
    (runs added : Nat) (nativeGrowth : added ≤ 2) :
    tick runs added ≤ 17 := by
  have h := an_eligible_group_is_small_after_a_check runs
  unfold tick
  omega

def run (runs : Nat) : List Nat → Nat
  | [] => runs
  | added :: rest => run (tick runs added) rest

theorem arbitrary_bounded_growth_histories_keep_at_most_17_runs
    (runs : Nat) (steps : List Nat) (initialBound : runs ≤ 17)
    (nativeGrowth : ∀ n ∈ steps, n ≤ 2) : run runs steps ≤ 17 := by
  induction steps generalizing runs with
  | nil => exact initialBound
  | cons n rest ih =>
    apply ih
    · exact every_prepend_bounds_even_an_arbitrarily_large_restored_group runs n
        (nativeGrowth n (by simp))
    · intro x hx
      exact nativeGrowth x (by simp [hx])

def replay (step : Archive → Input → Archive) (archive : Archive) (inputs : List Input) : Archive :=
  inputs.foldl step archive

theorem archive_determined_steps_replay_exactly_from_any_checkpoint
    (step : Archive → Input → Archive) (archive : Archive) (pre post : List Input) :
    replay step (replay step archive pre) post = replay step archive (pre ++ post) := by
  simp [replay, List.foldl_append]

/- A historical counterexample: persisting XML but resetting an external age
counter can choose different run layouts for the same next operation. -/
def oldScheduled (runs age added : Nat) : Nat :=
  (if 8 ≤ age then compactCount runs else runs) + added

theorem an_unpersisted_cadence_can_change_replayed_run_layout :
    oldScheduled 17 1 2 ≠ oldScheduled 17 8 2 := by decide

def publish (original candidate : Archive) (valid changed : Bool) : Archive :=
  if valid && changed then candidate else original

theorem a_failed_staged_native_input_never_publishes_normalization
    (original candidate : Archive) (changed : Bool) :
    publish original candidate false changed = original := by simp [publish]

theorem a_noop_preserves_the_exact_original_archive
    (original candidate : Archive) (valid : Bool) :
    publish original candidate valid false = original := by simp [publish]

end JadeCollab.NotesRuns
