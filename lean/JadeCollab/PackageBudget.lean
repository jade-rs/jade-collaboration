import Std

/-! Byte accounting for the engine's existing package loader. ZIP declarations
and actual output are checked independently. IO, XML parsing and total parser
heap use are abstracted; Rust tests check the loader and all hosted formats. -/
namespace JadeCollab.PackageBudget

def admit (part total used bytes : Nat) (complete : Bool) : Option Nat :=
  if complete && bytes ≤ part && used + bytes ≤ total then some (used + bytes) else none

def step (part total used bytes : Nat) (complete : Bool) : Nat :=
  (admit part total used bytes complete).getD used

def cap (part total used : Nat) : Nat := min part (total - used)

theorem admitted_part_and_total_are_bounded (part total used bytes accepted : Nat)
    (complete : Bool) (h : admit part total used bytes complete = some accepted) :
    complete = true ∧ bytes ≤ part ∧ accepted = used + bytes ∧ accepted ≤ total := by
  unfold admit at h
  split at h <;> simp_all

theorem accepted_part_fits_remaining_capacity (part total used bytes accepted : Nat)
    (complete : Bool) (bounded : used ≤ total)
    (h : admit part total used bytes complete = some accepted) : bytes ≤ cap part total used := by
  have := admitted_part_and_total_are_bounded part total used bytes accepted complete h
  simp [cap]
  omega

theorem failed_read_preserves_accounting (part total used bytes : Nat) :
    step part total used bytes false = used := by simp [step, admit]

theorem oversized_part_preserves_accounting (part total used bytes : Nat)
    (tooLarge : part < bytes) (complete : Bool) :
    step part total used bytes complete = used := by
  simp [step, admit, Nat.not_le.mpr tooLarge]

theorem total_overflow_preserves_accounting (part total used bytes : Nat)
    (tooLarge : total < used + bytes) (complete : Bool) :
    step part total used bytes complete = used := by
  simp [step, admit, Nat.not_le.mpr tooLarge]

theorem exact_cap_is_accepted (part total used : Nat) (bounded : used ≤ total) :
    admit part total used (cap part total used) true = some (used + cap part total used) := by
  have partBound : cap part total used ≤ part := Nat.min_le_left _ _
  have totalBound : used + cap part total used ≤ total := by
    have : cap part total used ≤ total - used := Nat.min_le_right _ _
    omega
  simp [admit, partBound, totalBound]

theorem cap_plus_one_is_refused (part total used : Nat) (bounded : used ≤ total) :
    admit part total used (cap part total used + 1) true = none := by
  unfold cap admit
  by_cases h : part ≤ total - used
  · simp [Nat.min_eq_left h] <;> omega
  · have : used + (min part (total - used) + 1) > total := by
      rw [Nat.min_eq_right (by omega)]
      omega
    simp [Nat.not_le.mpr this]

theorem successful_accounting_never_rewinds (part total used bytes : Nat) (complete : Bool) :
    used ≤ step part total used bytes complete := by
  unfold step admit
  split <;> simp_all <;> omega

theorem accounting_stays_bounded (part total used bytes : Nat) (complete : Bool)
    (bounded : used ≤ total) : step part total used bytes complete ≤ total := by
  unfold step admit
  split <;> simp_all

theorem histories_stay_bounded (part total : Nat) (parts : List (Nat × Bool))
    (used : Nat) (bounded : used ≤ total) :
    parts.foldl (fun used entry => step part total used entry.1 entry.2) used ≤ total := by
  induction parts generalizing used with
  | nil => exact bounded
  | cons entry rest ih =>
    exact ih _ (accounting_stays_bounded part total used entry.1 entry.2 bounded)

def metadata (archive entryLimit part total archiveBytes entries : Nat) (sizes : List Nat) : Prop :=
  archiveBytes ≤ archive ∧ entries ≤ entryLimit ∧ (∀ size ∈ sizes, size ≤ part) ∧ sizes.sum ≤ total

theorem declared_metadata_cannot_authorize_extra_actual_output :
    metadata 10 10 3 9 1 1 [1] ∧ admit 3 9 0 4 true = none := by simp [metadata, admit]

end JadeCollab.PackageBudget
