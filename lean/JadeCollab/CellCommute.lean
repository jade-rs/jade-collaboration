import JadeCollab.Grid

/-!
# Cell contents may travel the convergent lane

design.md §2 and §4 (Tables): structural table operations are sequenced, but an
edit to a cell's contents is convergent. A cell is keyed by `(RowId, ColId)`
and an edit is accepted iff both identifiers exist in the grid — alive *or
deleted* (`Grid::contains_row` / `contains_column`, as in
`jade-xlsx-collab`'s model). Because grid operations only ever add identifiers
(deletion tombstones), a cell edit that is valid where it was made stays valid
after any grid operation, and it commutes with that operation. Backs the
classification of cell edits in `jade-docx-collab` / `jade-xlsx-collab`.

The edit itself is any function of the cell's old contents (an RGA insert, a
last-writer-wins write, …); only *where* it lands matters here.
-/

namespace JadeCollab.CellCommute

open JadeCollab.Grid

/-- A table: its grid and the contents of every cell, deleted ones included. -/
structure Doc where
  grid : Grid
  cells : Nat → Nat → List Nat

/-- A grid operation leaves the cells as they are. -/
def gridOp (d : Doc) (op : Op) : Option Doc :=
  (apply d.grid op).map fun g => { d with grid := g }

def update (cells : Nat → Nat → List Nat) (r c : Nat) (f : List Nat → List Nat) :
    Nat → Nat → List Nat :=
  fun x y => if x = r ∧ y = c then f (cells r c) else cells x y

/-- A cell edit is accepted iff its row and column exist, alive or deleted. -/
def cellEdit (d : Doc) (r c : Nat) (f : List Nat → List Nat) : Option Doc :=
  if r ∈ ids d.grid.rows ∧ c ∈ ids d.grid.cols then some { d with cells := update d.cells r c f }
  else none

/-- The row identifier an operation creates, if any. -/
def _root_.JadeCollab.Grid.Op.newRow : Op → Option Nat
  | .insertRow id _ => some id
  | _ => none

/-- The column identifier an operation creates, if any. -/
def _root_.JadeCollab.Grid.Op.newCol : Op → Option Nat
  | .insertCol id _ => some id
  | _ => none

theorem mem_ids_insertAt {ls : List Line} {k : Nat} {e : Line} {x : Nat} :
    x ∈ ids (insertAt ls k e) ↔ x ∈ ids ls ∨ x = e.id := by
  rw [ids_insertAt]
  conv => rhs; rw [← List.take_append_drop k (ids ls)]
  simp only [List.mem_append, List.mem_cons]
  constructor
  · rintro (h | h | h) <;> simp_all
  · rintro ((h | h) | h) <;> simp_all

theorem insertLine_ids {ls rs : List Line} {id : Nat} {after : Option Nat}
    (h : insertLine ls id after = some rs) (x : Nat) : x ∈ ids rs ↔ x ∈ ids ls ∨ x = id := by
  obtain ⟨-, k, -, rfl⟩ := insertLine_spec h
  exact mem_ids_insertAt

theorem deleteLine_ids {ls rs : List Line} {id : Nat} (h : deleteLine ls id = some rs) :
    ids rs = ids ls := by
  obtain ⟨d, rfl, -⟩ := deleteLine_spec h
  exact ids_killAt _ _

/-- **Grid operations never remove identifiers**, and the only ones they add
are the row or column an insertion creates. -/
theorem apply_ids {g g' : Grid} {op : Op} (h : apply g op = some g') (x : Nat) :
    (x ∈ ids g'.rows ↔ x ∈ ids g.rows ∨ op.newRow = some x) ∧
      (x ∈ ids g'.cols ↔ x ∈ ids g.cols ∨ op.newCol = some x) := by
  cases op with
  | insertRow id after =>
    simp only [apply, Option.map_eq_some_iff] at h
    obtain ⟨rs, hrs, rfl⟩ := h
    simp [insertLine_ids hrs, Op.newRow, Op.newCol, eq_comm]
  | deleteRow id =>
    simp only [apply, Option.map_eq_some_iff] at h
    obtain ⟨rs, hrs, rfl⟩ := h
    simp [normalize, deleteLine_ids hrs, Op.newRow, Op.newCol]
  | insertCol id after =>
    simp only [apply, Option.map_eq_some_iff] at h
    obtain ⟨cs, hcs, rfl⟩ := h
    simp [insertLine_ids hcs, Op.newRow, Op.newCol, eq_comm]
  | deleteCol id =>
    simp only [apply, Option.map_eq_some_iff] at h
    obtain ⟨cs, hcs, rfl⟩ := h
    simp [normalize, deleteLine_ids hcs, Op.newRow, Op.newCol]
  | merge m =>
    simp only [apply] at h
    split at h
    · cases h; simp [Op.newRow, Op.newCol]
    · simp at h
  | unmerge t l =>
    simp only [apply] at h
    split at h
    · simp at h
    · cases h; simp [Op.newRow, Op.newCol]

theorem apply_ids_mono {g g' : Grid} {op : Op} (h : apply g op = some g') {x : Nat} :
    (x ∈ ids g.rows → x ∈ ids g'.rows) ∧ (x ∈ ids g.cols → x ∈ ids g'.cols) :=
  ⟨fun hx => ((apply_ids h x).1.mpr (Or.inl hx)), fun hx => ((apply_ids h x).2.mpr (Or.inl hx))⟩

/-- **A cell edit commutes with every grid operation.** If the edit is valid
where it was made, applying it before or after the grid operation gives the
same result: both orders are rejected (when the grid operation is), or both
succeed with the same table. -/
theorem cell_edit_comm_grid (d : Doc) (op : Op) (r c : Nat) (f : List Nat → List Nat)
    (hE : (cellEdit d r c f).isSome) :
    (cellEdit d r c f).bind (gridOp · op) = (gridOp d op).bind (cellEdit · r c f) := by
  by_cases hrc : r ∈ ids d.grid.rows ∧ c ∈ ids d.grid.cols
  · unfold cellEdit
    rw [ite_eq_left hrc, Option.bind_some]
    simp only [gridOp]
    cases hg : apply d.grid op with
    | none => simp
    | some g' =>
      have hm := apply_ids_mono hg (x := r)
      have hm' := apply_ids_mono hg (x := c)
      simp [hm.1 hrc.1, hm'.2 hrc.2]
  · simp [cellEdit, hrc] at hE

theorem cell_edit_after_grid {d d' : Doc} {op : Op} {r c : Nat} {f : List Nat → List Nat}
    (hE : (cellEdit d r c f).isSome) (h : gridOp d op = some d') : (cellEdit d' r c f).isSome := by
  simp only [gridOp, Option.map_eq_some_iff] at h
  obtain ⟨g', hg, rfl⟩ := h
  by_cases hrc : r ∈ ids d.grid.rows ∧ c ∈ ids d.grid.cols
  · simp [cellEdit, (apply_ids_mono hg).1 hrc.1, (apply_ids_mono hg).2 hrc.2]
  · simp [cellEdit, hrc] at hE

/-- **The edit is accepted after a grid operation iff it is accepted before**,
unless that operation is the one that created the edited row or column. -/
theorem cell_edit_after_grid_iff {d d' : Doc} {op : Op} (h : gridOp d op = some d')
    {r c : Nat} (hr : op.newRow ≠ some r) (hc : op.newCol ≠ some c) (f : List Nat → List Nat) :
    (cellEdit d' r c f).isSome ↔ (cellEdit d r c f).isSome := by
  simp only [gridOp, Option.map_eq_some_iff] at h
  obtain ⟨g', hg, rfl⟩ := h
  have h1 := (apply_ids hg r).1
  have h2 := (apply_ids hg c).2
  simp [cellEdit, h1, h2, hr, hc]

/-- …and a grid operation is accepted after a cell edit iff before, with the same
grid: cell edits do not touch the grid. -/
theorem grid_after_cell_edit {d d' : Doc} {r c : Nat} {f : List Nat → List Nat}
    (h : cellEdit d r c f = some d') (op : Op) :
    (gridOp d' op).map (·.grid) = (gridOp d op).map (·.grid) := by
  unfold cellEdit at h
  split at h
  · cases h; simp [gridOp, Option.map_map, Function.comp_def]
  · simp at h

/-- Table operations: structure (sequenced) and cell edits (convergent). -/
inductive TableOp
  | grid (op : Op)
  | edit (r c : Nat) (f : List Nat → List Nat)

def applyTable (d : Doc) : TableOp → Option Doc
  | .grid op => gridOp d op
  | .edit r c f => cellEdit d r c f

/-- **Sequencer steps commute** for a cell edit that is valid on the state and
any grid operation, whether the grid operation is accepted or rejected. -/
theorem step_comm_edit_grid (s : Doc) (r c : Nat) (f : List Nat → List Nat) (op : Op)
    (hE : (cellEdit s r c f).isSome) :
    Sequencer.step applyTable (Sequencer.step applyTable s (.edit r c f)) (.grid op) =
      Sequencer.step applyTable (Sequencer.step applyTable s (.grid op)) (.edit r c f) := by
  have hcomm := cell_edit_comm_grid s op r c f hE
  obtain ⟨s1, hs1⟩ := Option.isSome_iff_exists.mp hE
  simp only [Sequencer.step, applyTable, hs1, Option.getD_some]
  rw [hs1, Option.bind_some] at hcomm
  cases hg : gridOp s op with
  | none =>
    rw [hg, Option.bind_none] at hcomm
    simp [hcomm, hs1]
  | some s2 =>
    rw [hg, Option.bind_some] at hcomm
    obtain ⟨s3, hs3⟩ := Option.isSome_iff_exists.mp (cell_edit_after_grid hE hg)
    simp [hcomm, hs3]

/-- **The freshness side condition is needed**: an edit to a row that does not
exist yet is rejected, and accepted once the row is inserted. (A replica only
edits cells it has seen, so a concurrent insertion never creates its row.) -/
theorem cell_edit_needs_existing_row :
    (cellEdit ⟨testGrid 2 2, fun _ _ => []⟩ 9 101 (· ++ [7])).isSome = false ∧
      ((gridOp ⟨testGrid 2 2, fun _ _ => []⟩ (.insertRow 9 none)).bind
        (cellEdit · 9 101 (· ++ [7]))).isSome = true := by
  decide

/-- Deletion only tombstones: a concurrent edit to a cell of a deleted row still
lands (`deleted_rows_keep_anchoring_concurrent_inserts`). -/
theorem cell_edit_in_deleted_row :
    ((gridOp ⟨testGrid 3 1, fun _ _ => []⟩ (.deleteRow 2)).bind
      (cellEdit · 2 101 (· ++ [7]))).isSome = true := by
  decide

end JadeCollab.CellCommute
