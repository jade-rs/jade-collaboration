import Std
import JadeCollab.DerivedBatch

/-! Per-request table expansion budgets are admission policy, not a limit on
existing documents. Trusted replay retains the original operation meaning.
Sparse visibility models output size; Rust PBT checks actual grid ordering.
-/
namespace JadeCollab.TableWork

def checkedCells (wordMax limit rows columns : Nat) : Option Nat :=
  if rows * columns ≤ wordMax ∧ rows * columns ≤ limit then some (rows * columns) else none

theorem accepted_product_is_bounded (wordMax limit rows columns cells : Nat)
    (accepted : checkedCells wordMax limit rows columns = some cells) :
    cells = rows * columns ∧ cells ≤ wordMax ∧ cells ≤ limit := by
  unfold checkedCells at accepted
  split at accepted <;> simp_all

theorem exact_or_smaller_products_are_accepted (wordMax limit rows columns : Nat)
    (fitsWord : limit ≤ wordMax) (fits : rows * columns ≤ limit) :
    checkedCells wordMax limit rows columns = some (rows * columns) := by
  simp [checkedCells, fits, Nat.le_trans fits fitsWord]

theorem excessive_products_are_refused (wordMax limit rows columns : Nat)
    (tooLarge : limit < rows * columns) : checkedCells wordMax limit rows columns = none := by
  simp [checkedCells, Nat.not_le.mpr tooLarge]

theorem overflowing_products_are_refused (wordMax limit rows columns : Nat)
    (overflow : wordMax < rows * columns) : checkedCells wordMax limit rows columns = none := by
  simp [checkedCells, Nat.not_le.mpr overflow]

theorem zero_axis_does_not_overflow (wordMax limit rows : Nat) :
    checkedCells wordMax limit rows 0 = some 0 := by simp [checkedCells]

def admit {S Op : Type} (limit : Nat) (cost : Op → Nat) (apply : S → Op → Option S)
    (state : S) (op : Op) : Option S :=
  if cost op ≤ limit then apply state op else none

def publish {S : Type} (state : S) (planned : Option S) : S := planned.getD state

theorem resource_refusal_preserves_model_and_clock {S Op : Type}
    (limit : Nat) (cost : Op → Nat) (apply : S → Op → Option S) (state : S) (op : Op)
    (tooLarge : limit < cost op) : publish state (admit limit cost apply state op) = state := by
  simp [publish, admit, Nat.not_le.mpr tooLarge]

theorem admitted_and_recorded_operations_have_the_same_meaning {S Op : Type}
    (limit : Nat) (cost : Op → Nat) (apply : S → Op → Option S) (state : S) (op : Op)
    (fits : cost op ≤ limit) : admit limit cost apply state op = apply state op := by
  simp [admit, fits]

-- Recorded facts use logical application directly; a later admission policy
-- cannot invalidate an already committed operation, but logical errors remain.
theorem trusted_record_preserves_its_original_result {S Op : Type}
    (apply : S → Op → Option S) (state result : S) (op : Op)
    (valid : apply state op = some result) : publish state (apply state op) = result := by
  simp [publish, valid]

theorem invalid_record_does_not_change_state {S Op : Type}
    (apply : S → Op → Option S) (state : S) (op : Op)
    (invalid : apply state op = none) : publish state (apply state op) = state := by
  simp [publish, invalid]

theorem incremental_growth_can_exceed_the_per_operation_budget :
    checkedCells 65535 10000 100 100 = some 10000 ∧
    checkedCells 65535 10000 1 100 = some 100 ∧ 10000 < 101 * 100 := by decide

theorem zero_new_stories_need_no_identifier_headroom (limit : Nat) :
    DerivedBatch.allocate limit limit 0 = some limit := DerivedBatch.no_identifiers_needed_at_the_limit limit

def visible (rows columns : List Nat) (cells : List (Nat × Nat)) : List (Nat × Nat) :=
  cells.filter (fun cell => rows.contains cell.1 && columns.contains cell.2)

theorem sparse_output_is_bounded_by_stored_cells (rows columns : List Nat)
    (cells : List (Nat × Nat)) : (visible rows columns cells).length ≤ cells.length := by
  exact List.length_filter_le _ _

theorem tombstoned_axes_cannot_appear_in_visible_output (rows columns : List Nat)
    (cells : List (Nat × Nat)) (cell : Nat × Nat) (member : cell ∈ visible rows columns cells) :
    cell.1 ∈ rows ∧ cell.2 ∈ columns := by
  simp [visible] at member
  exact member.2

end JadeCollab.TableWork
