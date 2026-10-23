import Std

/-! Native paragraph surface ordinals can change between helper commands.
Each frame faithfully resolves a stable story address, but makes no promise
about keeping yesterday's ordinal. Rust resolves again after each placeholder
and edit, and resolves both endpoints of a join in that current frame.
Story identity preservation through these helper steps, native lookup fidelity,
offset conversion and native writes remain assumptions checked by native PBT. -/
namespace JadeCollab.StableSurfaces

structure Frame (Address : Type) where
  locate : Address → Nat
  atIndex : Nat → Option Address
  faithful : ∀ address, atIndex (locate address) = some address

def point (frame : Frame Address) (address : Address) (offset : Nat) : Nat × Nat :=
  (frame.locate address,offset)

theorem resolving_in_the_current_frame_targets_the_requested_story (frame : Frame Address)
    (address : Address) : frame.atIndex (frame.locate address) = some address :=
  frame.faithful address

theorem readdressing_preserves_the_logical_caret (frame : Frame Address) (address : Address) (offset : Nat) :
    (point frame address offset).2 = offset := rfl

theorem faithful_addresses_cannot_alias (frame : Frame Address) (a b : Address)
    (sameIndex : frame.locate a = frame.locate b) : a = b := by
  have same : some a = some b := by
    rw [← frame.faithful a, ← frame.faithful b, sameIndex]
  exact Option.some.inj same

def targets (frames : List (Frame Address)) (address : Address) : List (Option Address) :=
  frames.map (fun frame => frame.atIndex (frame.locate address))

theorem arbitrary_surface_reorderings_never_retarget_a_stable_address
    (frames : List (Frame Address)) (address : Address) :
    targets frames address = List.replicate frames.length (some address) := by
  induction frames with
  | nil => rfl
  | cons frame rest ih =>
    change frame.atIndex (frame.locate address) :: targets rest address =
      List.replicate (rest.length + 1) (some address)
    rw [frame.faithful, List.replicate_succ, ih]

def planJoin (frame : Frame Address) (first second : Address) : Option (Nat × Nat) :=
  if frame.locate first + 1 = frame.locate second then
    some (frame.locate first,frame.locate second)
  else none

theorem a_join_resolves_both_endpoints_in_the_current_frame (frame : Frame Address)
    (first second : Address) (adjacent : frame.locate first + 1 = frame.locate second) :
    (planJoin frame first second).map (fun (a,b) => (frame.atIndex a,frame.atIndex b)) =
      some (some first,some second) := by simp [planJoin,adjacent,frame.faithful]

theorem a_newly_nonadjacent_join_emits_no_command (frame : Frame Address) (first second : Address)
    (separated : frame.locate first + 1 ≠ frame.locate second) :
    planJoin frame first second = none := by simp [planJoin,separated]

def before : Frame Bool where
  locate address := if address then 1 else 0
  atIndex index := if index = 0 then some false else some true
  faithful := by intro address; cases address <;> rfl

def after : Frame Bool where
  locate address := if address then 0 else 1
  atIndex index := if index = 0 then some true else some false
  faithful := by intro address; cases address <;> rfl

theorem a_cached_ordinal_can_silently_select_another_story :
    after.atIndex (before.locate false) = some true ∧
    after.atIndex (after.locate false) = some false := by decide

theorem adjacency_must_be_checked_after_the_reordering :
    planJoin before false true = some (0,1) ∧ planJoin after false true = none := by decide

end JadeCollab.StableSurfaces
