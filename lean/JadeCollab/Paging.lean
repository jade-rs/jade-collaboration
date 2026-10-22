import JadeCollab.Lww

/-! A finite stream of sparse XLSX items, with one item per cell (value and
format together), axis style or merge range. This proves pagination of that
stream; Rust stateful tests check how document state produces the items. -/
namespace JadeCollab.Paging

def slice (items : List α) (start limit : Nat) : List α :=
  (items.drop start).take limit

theorem bounded (items : List α) (start limit : Nat) :
    (slice items start limit).length ≤ limit :=
  List.length_take_le limit _

/-- One page followed by the remaining suffix reconstructs every item in
the unread stream, in order, even when several items carry the same value. -/
theorem page_and_tail (items : List α) (start limit : Nat) :
    slice items start limit ++ items.drop (start + limit) = items.drop start := by
  simpa only [slice, List.drop_drop] using List.take_append_drop limit (items.drop start)

def finish (total start limit : Nat) : Nat := min total (start + limit)

def next (total start limit : Nat) : Option Nat :=
  let ending := finish total start limit
  if ending < total then some ending else none

/-- Positive limits make every continuation advance, so an unchanged finite
stream cannot loop forever. -/
theorem next_progress (total start limit cursor : Nat) (hl : 0 < limit)
    (hn : next total start limit = some cursor) :
    start < cursor ∧ cursor < total := by
  by_cases h : start + limit < total
  · have he : min total (start + limit) = start + limit := Nat.min_eq_right (by omega)
    simp [next, finish, he, h] at hn
    omega
  · have he : min total (start + limit) = total := Nat.min_eq_left (by omega)
    simp [next, finish, he] at hn

theorem next_at_end (total start limit : Nat) (hs : total ≤ start) :
    next total start limit = none := by
  simp [next, finish, Nat.min_eq_left (show total ≤ start + limit by omega)]

/-- Rust uses saturating usize addition. Once total fits in usize, capping
the sum before taking the minimum produces the same page boundary. -/
theorem saturating_finish (cap total start limit : Nat) (ht : total ≤ cap) :
    min total (min cap (start + limit)) = finish total start limit := by
  unfold finish
  omega

end JadeCollab.Paging
