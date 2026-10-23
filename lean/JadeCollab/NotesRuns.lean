import Std

/-! Notes run maintenance preserves the paragraph's styled character stream and
opaque XML anchors. Parsing/namespaces, raw XML joins and native Office rendering
are Rust/native-PBT boundaries, not assumed to be proved by this model.
The count bound is for one eligible group under successful text-changing native
prepends adding at most two runs, checked every eight changes. Rich/opaque
barriers remain separate. Publication lemmas concern the native archive only. -/
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

structure State where
  runs : Nat
  inputsSinceCheck : Nat

def tick (s : State) (added : Nat) : State :=
  if 8 ≤ s.inputsSinceCheck then
    ⟨compactCount s.runs + added, 1⟩
  else ⟨s.runs + added, s.inputsSinceCheck + 1⟩

def Bounded (s : State) : Prop :=
  s.inputsSinceCheck ≤ 8 ∧ s.runs ≤ 15 + 2 * s.inputsSinceCheck

theorem the_first_check_bounds_an_arbitrarily_large_restored_group
    (runs added : Nat) (nativeGrowth : added ≤ 2) :
    Bounded (tick ⟨runs,8⟩ added) := by
  have h := an_eligible_group_is_small_after_a_check runs
  simp [tick, Bounded]
  omega

theorem bounded_native_growth_preserves_the_maintenance_invariant
    (s : State) (added : Nat) (bounded : Bounded s) (nativeGrowth : added ≤ 2) :
    Bounded (tick s added) := by
  have h := an_eligible_group_is_small_after_a_check s.runs
  unfold tick
  split <;> simp only [Bounded] at * <;> omega

theorem repeated_prepends_never_leave_more_than_31_runs
    (s : State) (bounded : Bounded s) : s.runs ≤ 31 := by
  unfold Bounded at bounded
  omega

def run (s : State) : List Nat → State
  | [] => s
  | added :: rest => run (tick s added) rest

theorem arbitrary_bounded_growth_histories_remain_bounded
    (s : State) (steps : List Nat) (bounded : Bounded s)
    (nativeGrowth : ∀ n ∈ steps, n ≤ 2) : Bounded (run s steps) := by
  induction steps generalizing s with
  | nil => exact bounded
  | cons n rest ih =>
    apply ih
    · exact bounded_native_growth_preserves_the_maintenance_invariant s n bounded
        (nativeGrowth n (by simp))
    · intro x hx
      exact nativeGrowth x (by simp [hx])

def publish (original candidate : Archive) (valid changed : Bool) : Archive :=
  if valid && changed then candidate else original

theorem a_failed_staged_native_input_never_publishes_normalization
    (original candidate : Archive) (changed : Bool) :
    publish original candidate false changed = original := by simp [publish]

theorem a_noop_preserves_the_exact_original_archive
    (original candidate : Archive) (valid : Bool) :
    publish original candidate valid false = original := by simp [publish]

end JadeCollab.NotesRuns
