import JadeCollab.Lww

/-! Source XLSX styles are complete layers; later edits are per-attribute
writes. `0` encodes an unresolved attribute, not an absent register. -/
namespace JadeCollab.Style
open Lww

/-- Source style selection precedes attribute lookup, so an automatic or
unresolved cell attribute cannot inherit a row's resolved colour. -/
def source (fallback : Nat) (column row cell : Option Nat) : Nat :=
  cell.getD (row.getD (column.getD fallback))

theorem explicit_cell_wins (fallback column row value) :
    source fallback column row (some value) = value := rfl

theorem row_wins_without_cell (fallback column value) :
    source fallback column (some value) none = value := rfl

theorem unresolved_cell_hides_row (fallback column row) :
    source fallback column row (some 0) = 0 := rfl

/-- The effective register for one attribute, as in Sheet.effective. -/
def effective (base cell row column : Reg) : Reg :=
  merge base (merge cell (merge row column))

theorem update_cell (base cell row column : Reg) (w : Write) :
    effective base (write cell w) row column =
      write (effective base cell row column) w := by
  simp only [effective, write]
  rw [merge_assoc, merge_comm (some w), ← merge_assoc, ← merge_assoc]
  simp only [merge_assoc]

theorem update_row (base cell row column : Reg) (w : Write) :
    effective base cell (write row w) column =
      write (effective base cell row column) w := by
  simp only [effective, write]
  rw [merge_assoc row, merge_comm (some w), ← merge_assoc row]
  simp only [merge_assoc]

theorem update_column (base cell row column : Reg) (w : Write) :
    effective base cell row (write column w) =
      write (effective base cell row column) w := by
  simp only [effective, write, merge_assoc]

/-- An update's visible result is independent of the row/cell layer in
which it is stored, hence cross-layer concurrent writes also commute. -/
theorem cross_layer_commute (base cell row column : Reg) (a b : Write) :
    effective base (write cell a) (write row b) column =
      write (write (effective base cell row column) a) b := by
  rw [update_cell, update_row, write_comm]

/-- All imported priorities are below the first post-import clock tick.
This includes an unresolved attribute encoded by zero. -/
theorem fresh_overrides_source (baseline priority value lamport replica newer : Nat)
    (hp : priority ≤ 4) (hf : baseline + 4 < lamport) :
    write (some (baseline + priority, 0, value)) (lamport, replica, newer) =
      some (lamport, replica, newer) := by
  simp [write, merge, maxW, lt, show baseline + priority < lamport by omega]

end JadeCollab.Style
