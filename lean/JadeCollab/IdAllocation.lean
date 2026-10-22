/-! Bounded identifiers: accepted runs never wrap, reservations are newer
than all observed time, and rejected reservations leave time unchanged.
The executable oracle uses this same model, with the Rust u64 bound. -/
namespace JadeCollab.IdAllocation

def u64Max : Nat := 18446744073709551615

theorem u64Max_eq : u64Max = 2 ^ 64 - 1 := by decide

def runEnd (limit first count : Nat) : Option Nat :=
  if count = 0 then none
  else if first + (count - 1) ≤ limit then some (first + (count - 1)) else none

theorem runEnd_some_iff (limit first count last : Nat) :
    runEnd limit first count = some last ↔
      0 < count ∧ last = first + (count - 1) ∧ last ≤ limit := by
  by_cases h0 : count = 0
  · simp [runEnd, h0]
  · by_cases hb : first + (count - 1) ≤ limit
    · simp [runEnd, h0, hb]; omega
    · simp [runEnd, h0, hb]; omega

theorem accepted_run_no_wrap (limit first count last : Nat)
    (h : runEnd limit first count = some last) (k : Nat) (hk : k < count) :
    first ≤ first + k ∧ first + k ≤ last ∧ last ≤ limit := by
  have := (runEnd_some_iff limit first count last).mp h
  omega

def reserve (limit time count : Nat) : Option (Nat × Nat) :=
  if count = 0 then none
  else if time + count ≤ limit then some (time + 1, time + count) else none

theorem reserve_some_iff (limit time count first last : Nat) :
    reserve limit time count = some (first, last) ↔
      0 < count ∧ first = time + 1 ∧ last = time + count ∧ last ≤ limit := by
  by_cases h0 : count = 0
  · simp [reserve, h0]
  · by_cases hb : time + count ≤ limit
    · simp [reserve, h0, hb]; omega
    · simp [reserve, h0, hb]; omega

theorem reserved_ids_are_fresh (limit time count first last : Nat)
    (h : reserve limit time count = some (first, last)) (k : Nat) (hk : k < count) :
    time < first + k ∧ first + k ≤ last ∧ last ≤ limit := by
  have := (reserve_some_iff limit time count first last).mp h
  omega

theorem reservation_has_valid_range (limit time count first last : Nat)
    (h : reserve limit time count = some (first, last)) :
    runEnd limit first count = some last := by
  have := (reserve_some_iff limit time count first last).mp h
  apply (runEnd_some_iff limit first count last).mpr
  omega

def nextTime (limit time count : Nat) : Nat :=
  match reserve limit time count with
  | none => time
  | some (_, last) => last

theorem rejected_reservation_unchanged (limit time count : Nat)
    (h : reserve limit time count = none) : nextTime limit time count = time := by
  simp [nextTime, h]

end JadeCollab.IdAllocation
