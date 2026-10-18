import JadeCollab.Sequencer

/-!
# The table grid, as implemented

A line-for-line model of `jade_collab_core::grid::Grid`
(`crates/jade-collab-core/src/grid.rs`) with `Nat` identifiers: `apply` takes
the same accept/reject decisions as `Grid::apply` and produces the same rows,
columns and merges in the same order; `Valid` is exactly what
`Grid::check_invariants` checks. Only which error is reported is not modelled.

Main result: `apply_preserves_valid`, and through
`Sequencer.sequenced_preserves`, `grid_sequenced_valid`. The executable
`check` is proven equivalent to `Valid` (`check_iff_valid`) and, together with
`apply` and `mergeAt`, drives the `grid-oracle` executable (`GridOracle.lean`).
Rows and columns go through merged regions as in Word and Excel
(`insertRow_covers`, `insertCol_covers`, `deleteRow_covers`,
`deleteCol_covers`).
-/

namespace JadeCollab.Grid

/-! ## The model (mirrors `crates/jade-collab-core/src/grid.rs`) -/

/-- A row or a column: its identifier and whether it is still alive. -/
structure Line where
  id : Nat
  alive : Bool
deriving DecidableEq, Repr

/-- A merged region named by its corner rows and columns (inclusive). -/
structure Merge where
  top : Nat
  left : Nat
  bottom : Nat
  right : Nat
deriving DecidableEq, Repr

structure Grid where
  rows : List Line
  cols : List Line
  merges : List Merge
deriving DecidableEq, Repr

inductive Op
  | insertRow (id : Nat) (after : Option Nat)
  | deleteRow (id : Nat)
  | insertCol (id : Nat) (after : Option Nat)
  | deleteCol (id : Nat)
  | merge (m : Merge)
  | unmerge (top left : Nat)
deriving DecidableEq, Repr

/-- `Grid::new`: every line alive, no merges. -/
def Grid.new (rows cols : List Nat) : Grid :=
  ⟨rows.map (⟨·, true⟩), cols.map (⟨·, true⟩), []⟩

def ids (ls : List Line) : List Nat := ls.map (·.id)

/-- `position`: index of the first line with this identifier. -/
def pos : List Line → Nat → Option Nat
  | [], _ => none
  | l :: ls, x => if l.id = x then some 0 else (pos ls x).map (· + 1)

/-- `lines[i].alive` (false out of range, which `apply` never asks). -/
def isAlive (ls : List Line) (i : Nat) : Bool :=
  match ls[i]? with
  | some l => l.alive
  | none => false

/-- `alive_position`: the position of an existing, alive line. -/
def alivePos (ls : List Line) (x : Nat) : Option Nat :=
  match pos ls x with
  | some i => if isAlive ls i then some i else none
  | none => none

/-- `lines[a..=b]`. -/
def slice (ls : List Line) (a b : Nat) : List Line := (ls.take (b + 1)).drop a

def countAlive (ls : List Line) : Nat := ls.countP (·.alive)

/-- The `count` closure of `Grid::cell_count`: alive lines between two
identifiers, 0 if one is missing or they are out of order. -/
def cnt (ls : List Line) (x y : Nat) : Nat :=
  match pos ls x, pos ls y with
  | some a, some b => if a ≤ b then countAlive (slice ls a b) else 0
  | _, _ => 0

def cellCount (g : Grid) (m : Merge) : Nat :=
  cnt g.rows m.top m.bottom * cnt g.cols m.left m.right

/-- The corner checks of `check_merge` on one axis: both corners exist and
are alive, and the first does not come after the second. -/
def axisOk (ls : List Line) (x y : Nat) : Bool :=
  match alivePos ls x, alivePos ls y with
  | some a, some b => decide (a ≤ b)
  | _, _ => false

/-- `Grid::check_merge` (`Ok` ↔ `true`). -/
def checkMerge (g : Grid) (m : Merge) : Bool :=
  axisOk g.rows m.top m.bottom && axisOk g.cols m.left m.right && decide (2 ≤ cellCount g m)

/-- The `span` closure of `Grid::overlaps`. Rust uses `usize::MAX` for a
missing first corner; `length + 1` behaves identically, since the only thing
it is compared with is a position or `0`, both `< length + 1`. -/
def span (ls : List Line) (x y : Nat) : Nat × Nat :=
  ((pos ls x).getD (ls.length + 1), (pos ls y).getD 0)

def inter (a b : Nat × Nat) : Bool := decide (a.1 ≤ b.2) && decide (b.1 ≤ a.2)

/-- `Grid::overlaps`, on raw positions. -/
def overlaps (g : Grid) (a b : Merge) : Bool :=
  inter (span g.rows a.top a.bottom) (span g.rows b.top b.bottom) &&
    inter (span g.cols a.left a.right) (span g.cols b.left b.right)

/-- `Vec::insert(k, e)` (`k ≤ length` whenever `apply` calls it). -/
def insertAt (ls : List Line) (k : Nat) (e : Line) : List Line :=
  ls.take k ++ e :: ls.drop k

/-- `Grid::insert` on one axis. -/
def insertLine (ls : List Line) (id : Nat) (after : Option Nat) : Option (List Line) :=
  if ls.any (·.id == id) then none
  else
    match after with
    | none => some (insertAt ls 0 ⟨id, true⟩)
    | some a =>
      match pos ls a with
      | none => none
      | some i => some (insertAt ls (i + 1) ⟨id, true⟩)

/-- `lines[i].alive = false`. -/
def killAt : List Line → Nat → List Line
  | [], _ => []
  | l :: ls, 0 => { l with alive := false } :: ls
  | l :: ls, n + 1 => l :: killAt ls n

/-- `Grid::delete` on one axis, before `normalize_merges`. -/
def deleteLine (ls : List Line) (id : Nat) : Option (List Line) :=
  match pos ls id with
  | none => none
  | some i =>
    if !isAlive ls i then none
    else if countAlive ls = 1 then none
    else some (killAt ls i)

/-- `alive_span`: first and last alive line between two identifiers. -/
def aliveSpan (ls : List Line) (x y : Nat) : Option (Nat × Nat) :=
  match pos ls x, pos ls y with
  | some a, some b =>
    if b < a then none
    else
      match ((slice ls a b).filter (·.alive)).head?, ((slice ls a b).filter (·.alive)).getLast? with
      | some f, some l => some (f.id, l.id)
      | _, _ => none
  | _, _ => none

/-- One step of the `filter_map` in `normalize_merges`. -/
def normalizeOne (g : Grid) (m : Merge) : Option Merge :=
  match aliveSpan g.rows m.top m.bottom, aliveSpan g.cols m.left m.right with
  | some (t, b), some (l, r) =>
    if 2 ≤ cellCount g ⟨t, l, b, r⟩ then some ⟨t, l, b, r⟩ else none
  | _, _ => none

/-- `Grid::normalize_merges`. -/
def normalize (g : Grid) : Grid :=
  { g with merges := g.merges.filterMap (normalizeOne g) }

/-- `Grid::apply`: `none` is `Err(_)` (and the grid is left as it was). -/
def apply (g : Grid) : Op → Option Grid
  | .insertRow id after => (insertLine g.rows id after).map fun rs => { g with rows := rs }
  | .deleteRow id => (deleteLine g.rows id).map fun rs => normalize { g with rows := rs }
  | .insertCol id after => (insertLine g.cols id after).map fun cs => { g with cols := cs }
  | .deleteCol id => (deleteLine g.cols id).map fun cs => normalize { g with cols := cs }
  | .merge m =>
    if checkMerge g m && !(g.merges.any (overlaps g m)) then
      some { g with merges := g.merges ++ [m] }
    else none
  | .unmerge t l =>
    match g.merges.findIdx? (fun m => m.top == t && m.left == l) with
    | none => none
    | some i => some { g with merges := g.merges.eraseIdx i }

/-- `Grid::check_invariants` as a proposition. Merges are compared with every
earlier one, `overlaps(later, earlier)`, exactly as the Rust loop does. -/
structure Valid (g : Grid) : Prop where
  rowsNodup : (ids g.rows).Nodup
  colsNodup : (ids g.cols).Nodup
  rowAlive : g.rows.any (·.alive) = true
  colAlive : g.cols.any (·.alive) = true
  mergesOk : ∀ m ∈ g.merges, checkMerge g m = true
  disjoint : g.merges.Pairwise (fun a b => overlaps g b a = false)

/-! ## Basic facts about positions -/

theorem pos_lt {ls : List Line} {x i : Nat} (h : pos ls x = some i) : i < ls.length := by
  induction ls generalizing i with
  | nil => simp [pos] at h
  | cons l ls ih =>
    simp only [pos] at h
    split at h
    · cases h; simp
    · obtain ⟨j, hj, rfl⟩ := Option.map_eq_some_iff.mp h
      have := ih hj; simp; omega

theorem pos_get {ls : List Line} {x i : Nat} (h : pos ls x = some i) :
    ∃ l, ls[i]? = some l ∧ l.id = x := by
  induction ls generalizing i with
  | nil => simp [pos] at h
  | cons l ls ih =>
    simp only [pos] at h
    split at h
    · cases h; exact ⟨l, rfl, by assumption⟩
    · obtain ⟨j, hj, rfl⟩ := Option.map_eq_some_iff.mp h
      simpa using ih hj

theorem pos_eq_none {ls : List Line} {x : Nat} : pos ls x = none ↔ x ∉ ids ls := by
  induction ls with
  | nil => simp [pos, ids]
  | cons l ls ih =>
    simp only [pos, ids, List.map_cons, List.mem_cons] at *
    split
    · simp_all
    · rw [Option.map_eq_none_iff, ih]; simp_all [eq_comm]

theorem pos_isSome {ls : List Line} {x : Nat} : (pos ls x).isSome ↔ x ∈ ids ls := by
  have := (pos_eq_none (ls := ls) (x := x))
  cases h : pos ls x <;> simp_all

theorem mem_ids_of_pos {ls : List Line} {x i : Nat} (h : pos ls x = some i) : x ∈ ids ls :=
  pos_isSome.mp (by simp [h])

theorem pos_of_get {ls : List Line} (hn : (ids ls).Nodup) {i : Nat} {l : Line}
    (h : ls[i]? = some l) : pos ls l.id = some i := by
  induction ls generalizing i with
  | nil => simp at h
  | cons h0 ls ih =>
    simp only [ids, List.map_cons, List.nodup_cons] at hn
    cases i with
    | zero => simp at h; subst h; simp [pos]
    | succ i =>
      simp at h
      have hne : h0.id ≠ l.id := by
        intro he; apply hn.1; rw [he]; exact List.mem_map_of_mem (f := (·.id)) (List.mem_of_getElem? h)
      simp [pos, hne, ih hn.2 h]

theorem pos_congr {ls ls' : List Line} (h : ids ls = ids ls') (x : Nat) : pos ls x = pos ls' x := by
  induction ls generalizing ls' with
  | nil => cases ls' <;> simp_all [ids, pos]
  | cons l ls ih =>
    cases ls' with
    | nil => simp [ids] at h
    | cons l' ls' =>
      simp only [ids, List.map_cons, List.cons.injEq] at h
      simp only [pos, h.1, ih h.2]


/-! ### Insertion shifts positions -/

/-- Where position `i` goes when a line is inserted at `k`. -/
def sh (k i : Nat) : Nat := if i < k then i else i + 1

theorem sh_le_sh {k a b : Nat} : sh k a ≤ sh k b ↔ a ≤ b := by
  unfold sh; split <;> split <;> omega

theorem insertAt_zero (ls : List Line) (e : Line) : insertAt ls 0 e = e :: ls := by
  simp [insertAt]

theorem insertAt_succ (l : Line) (ls : List Line) (k : Nat) (e : Line) :
    insertAt (l :: ls) (k + 1) e = l :: insertAt ls k e := by
  simp [insertAt]

theorem pos_insertAt {ls : List Line} {k : Nat} {e : Line} {x : Nat} (hk : k ≤ ls.length)
    (hx : e.id ≠ x) : pos (insertAt ls k e) x = (pos ls x).map (sh k) := by
  induction ls generalizing k with
  | nil =>
    simp at hk; subst hk
    simp [insertAt_zero, pos, hx]
  | cons l ls ih =>
    cases k with
    | zero =>
      rw [insertAt_zero]
      simp only [pos, hx, ite_false]
      split
      · simp [sh]
      · simp [Option.map_map]; rfl
    | succ k =>
      rw [insertAt_succ]
      simp at hk
      simp only [pos]
      split
      · simp [sh]
      · rw [ih (by omega), Option.map_map, Option.map_map]
        congr 1; funext i; by_cases hi : i < k <;> simp [sh, hi]

theorem get_insertAt {ls : List Line} {k : Nat} {e : Line} (hk : k ≤ ls.length) (i : Nat) :
    (insertAt ls k e)[sh k i]? = ls[i]? := by
  induction ls generalizing k i with
  | nil => simp at hk; subst hk; simp [insertAt_zero, sh]
  | cons l ls ih =>
    cases k with
    | zero => simp [insertAt_zero, sh]
    | succ k =>
      rw [insertAt_succ]
      simp at hk
      cases i with
      | zero => simp [sh]
      | succ i =>
        have := ih (k := k) (by omega) i
        have e1 : sh (k + 1) (i + 1) = sh k i + 1 := by unfold sh; grind
        rw [e1]; simpa using this

theorem isAlive_insertAt {ls : List Line} {k : Nat} {e : Line} (hk : k ≤ ls.length) (i : Nat) :
    isAlive (insertAt ls k e) (sh k i) = isAlive ls i := by
  simp [isAlive, get_insertAt hk]

theorem alivePos_insertAt {ls : List Line} {k : Nat} {e : Line} {x : Nat} (hk : k ≤ ls.length)
    (hx : e.id ≠ x) : alivePos (insertAt ls k e) x = (alivePos ls x).map (sh k) := by
  unfold alivePos
  rw [pos_insertAt hk hx]
  cases pos ls x with
  | none => rfl
  | some i => simp [isAlive_insertAt hk]

theorem axisOk_insertAt {ls : List Line} {k : Nat} {e : Line} {x y : Nat} (hk : k ≤ ls.length)
    (hx : e.id ≠ x) (hy : e.id ≠ y) : axisOk (insertAt ls k e) x y = axisOk ls x y := by
  unfold axisOk
  rw [alivePos_insertAt hk hx, alivePos_insertAt hk hy]
  cases alivePos ls x <;> cases alivePos ls y <;> simp [sh_le_sh]

theorem slice_insertAt {ls : List Line} {k a b : Nat} {e : Line} (hk : k ≤ ls.length)
    (hab : a ≤ b) :
    (slice ls a b).Sublist (slice (insertAt ls k e) (sh k a) (sh k b)) := by
  unfold slice insertAt sh
  by_cases h1 : b < k
  · have h2 : a < k := by omega
    simp only [h1, h2, ite_true]
    rw [List.take_append_of_le_length (by simp; omega), List.take_take, Nat.min_eq_left (by omega)]
    exact List.Sublist.refl _
  · by_cases h2 : a < k
    · simp only [h1, h2, ite_true, ite_false]
      have hlen : (ls.take k).length = k := by simp; omega
      have e1 : b + 1 + 1 - k = (b + 1 - k) + 1 := by omega
      rw [List.take_append, hlen, e1, List.take_take, Nat.min_eq_right (by omega),
        List.take_succ_cons, List.drop_append_of_le_length (by omega)]
      have e2 : ls.take (b + 1) = ls.take k ++ (ls.drop k).take (b + 1 - k) := by
        rw [← List.take_add]; congr 1; omega
      rw [e2, List.drop_append_of_le_length (by omega)]
      exact List.Sublist.append_left (List.sublist_cons_self _ _) _
    · simp only [h1, h2, ite_false]
      have hlen : (ls.take k).length = k := by simp; omega
      have e2 : ls.take (b + 1) = ls.take k ++ (ls.drop k).take (b + 1 - k) := by
        rw [← List.take_add]; congr 1; omega
      have L : (ls.take (b + 1)).drop a = ((ls.drop k).take (b + 1 - k)).drop (a - k) := by
        rw [e2, List.drop_append, hlen, List.drop_eq_nil_of_le (by simp; omega), List.nil_append]
      have R : ((ls.take k ++ e :: ls.drop k).take (b + 1 + 1)).drop (a + 1)
          = ((ls.drop k).take (b + 1 - k)).drop (a - k) := by
        have e1 : b + 1 + 1 - k = (b + 1 - k) + 1 := by omega
        have e3 : a + 1 - k = (a - k) + 1 := by omega
        rw [List.take_append, hlen, e1, List.take_take, Nat.min_eq_right (by omega),
          List.take_succ_cons, List.drop_append, hlen, List.drop_eq_nil_of_le (by simp; omega),
          List.nil_append, e3, List.drop_succ_cons]
      rw [L, R]
      exact List.Sublist.refl _

theorem cnt_insertAt {ls : List Line} {k : Nat} {e : Line} {x y : Nat} (hk : k ≤ ls.length)
    (hx : e.id ≠ x) (hy : e.id ≠ y) : cnt ls x y ≤ cnt (insertAt ls k e) x y := by
  unfold cnt
  rw [pos_insertAt hk hx, pos_insertAt hk hy]
  cases pos ls x <;> cases pos ls y <;> simp only [Option.map_some, Option.map_none, Nat.le_refl]
  rename_i a b
  by_cases hab : a ≤ b
  · simp only [hab, ite_true, sh_le_sh.mpr hab]
    exact (slice_insertAt hk hab).countP_le
  · simp [hab]

theorem span_of_pos {ls : List Line} {x y a b : Nat} (ha : pos ls x = some a)
    (hb : pos ls y = some b) : span ls x y = (a, b) := by
  simp [span, ha, hb]

theorem inter_sh (k : Nat) (a1 b1 a2 b2 : Nat) :
    inter (sh k a1, sh k b1) (sh k a2, sh k b2) = inter (a1, b1) (a2, b2) := by
  simp [inter, sh_le_sh]

theorem ids_insertAt (ls : List Line) (k : Nat) (e : Line) :
    ids (insertAt ls k e) = (ids ls).take k ++ e.id :: (ids ls).drop k := by
  simp [ids, insertAt, List.map_take, List.map_drop]

theorem nodup_insertAt {ls : List Line} {k : Nat} {e : Line} (hn : (ids ls).Nodup)
    (he : e.id ∉ ids ls) : (ids (insertAt ls k e)).Nodup := by
  rw [ids_insertAt, List.perm_middle.nodup_iff, List.take_append_drop]
  exact List.nodup_cons.mpr ⟨he, hn⟩

theorem any_alive_insertAt {ls : List Line} {k : Nat} {e : Line}
    (h : ls.any (·.alive) = true) : (insertAt ls k e).any (·.alive) = true := by
  rw [← List.take_append_drop k ls] at h
  simp only [List.any_append] at h
  simp only [insertAt, List.any_append, List.any_cons]
  cases h1 : (ls.take k).any (·.alive) <;> simp_all

/-! ### Deletion only tombstones -/

theorem ids_killAt (ls : List Line) (d : Nat) : ids (killAt ls d) = ids ls := by
  induction ls generalizing d with
  | nil => simp [killAt]
  | cons l ls ih => cases d <;> simp [killAt, ids] at * <;> exact ih _

theorem length_killAt (ls : List Line) (d : Nat) : (killAt ls d).length = ls.length := by
  have := congrArg List.length (ids_killAt ls d); simpa [ids] using this

theorem countAlive_killAt {ls : List Line} {d : Nat} (h : isAlive ls d = true) :
    countAlive (killAt ls d) + 1 = countAlive ls := by
  induction ls generalizing d with
  | nil => simp [isAlive] at h
  | cons l ls ih =>
    cases d with
    | zero => simp [isAlive] at h; simp [killAt, countAlive, h]
    | succ d =>
      simp [isAlive] at h
      have := ih (d := d) (by simp [isAlive, h])
      simp [killAt, countAlive, List.countP_cons] at this ⊢; omega

theorem any_alive_iff (ls : List Line) : ls.any (·.alive) = true ↔ 0 < countAlive ls := by
  simp [countAlive]

/-! ### Normalization keeps corners inside the old region -/

theorem mem_slice {ls : List Line} {a b : Nat} {l : Line} (h : l ∈ slice ls a b) :
    ∃ i, a ≤ i ∧ i ≤ b ∧ ls[i]? = some l := by
  obtain ⟨n, hn⟩ := List.mem_iff_getElem?.mp h
  simp only [slice, List.getElem?_drop, List.getElem?_take] at hn
  split at hn
  · exact ⟨a + n, by omega, by omega, hn⟩
  · simp at hn

theorem alive_in_slice {ls : List Line} (hn : (ids ls).Nodup) {a b : Nat} {l : Line}
    (h : l ∈ (slice ls a b).filter (·.alive)) :
    ∃ i, a ≤ i ∧ i ≤ b ∧ pos ls l.id = some i ∧ isAlive ls i = true := by
  rw [List.mem_filter] at h
  obtain ⟨i, h1, h2, h3⟩ := mem_slice h.1
  exact ⟨i, h1, h2, pos_of_get hn h3, by simp [isAlive, h3, h.2]⟩

theorem aliveSpan_spec {ls : List Line} (hn : (ids ls).Nodup) {x y u v : Nat}
    (h : aliveSpan ls x y = some (u, v)) :
    ∃ a b i j, pos ls x = some a ∧ pos ls y = some b ∧ pos ls u = some i ∧ pos ls v = some j ∧
      a ≤ i ∧ i ≤ b ∧ a ≤ j ∧ j ≤ b ∧ isAlive ls i = true ∧ isAlive ls j = true := by
  unfold aliveSpan at h
  split at h
  · rename_i a b ha hb
    split at h
    · simp at h
    · split at h
      · rename_i f l hf hl
        simp only [Option.some.injEq, Prod.mk.injEq] at h
        obtain ⟨rfl, rfl⟩ := h
        obtain ⟨i, hi1, hi2, hi3, hi4⟩ := alive_in_slice hn (List.mem_of_mem_head? hf)
        obtain ⟨j, hj1, hj2, hj3, hj4⟩ := alive_in_slice hn (List.mem_of_getLast? hl)
        exact ⟨a, b, i, j, ha, hb, hi3, hj3, hi1, hi2, hj1, hj2, hi4, hj4⟩
      · simp at h
  · simp at h

theorem pos_le_of_cnt {ls : List Line} {x y : Nat} (h : cnt ls x y ≠ 0) :
    ∃ a b, pos ls x = some a ∧ pos ls y = some b ∧ a ≤ b := by
  unfold cnt at h
  split at h
  · rename_i a b ha hb
    split at h
    · exact ⟨a, b, ha, hb, by assumption⟩
    · simp at h
  · simp at h

theorem span_eq_of_ids {ls ls' : List Line} (h : ids ls = ids ls') (x y : Nat) :
    span ls x y = span ls' x y := by
  have hl : ls.length = ls'.length := by simpa [ids] using congrArg List.length h
  simp [span, pos_congr h, hl]

/-! ### Unfolding the checks -/

theorem axisOk_spec {ls : List Line} {x y : Nat} :
    axisOk ls x y = true ↔ ∃ a b, pos ls x = some a ∧ pos ls y = some b ∧
      isAlive ls a = true ∧ isAlive ls b = true ∧ a ≤ b := by
  unfold axisOk alivePos
  cases ha : pos ls x with
  | none => simp
  | some a =>
    cases hb : pos ls y with
    | none => simp
    | some b => cases hla : isAlive ls a <;> cases hlb : isAlive ls b <;> simp [hla, hlb]

theorem insertLine_spec {ls rs : List Line} {id : Nat} {after : Option Nat}
    (h : insertLine ls id after = some rs) :
    id ∉ ids ls ∧ ∃ k, k ≤ ls.length ∧ rs = insertAt ls k ⟨id, true⟩ := by
  unfold insertLine at h
  split at h
  · simp at h
  · rename_i hd
    have hid : id ∉ ids ls := by
      intro hm; apply hd; simp [ids] at hm; obtain ⟨l, hl, rfl⟩ := hm; simp; exact ⟨l, hl, rfl⟩
    refine ⟨hid, ?_⟩
    split at h
    · cases h; exact ⟨0, by omega, rfl⟩
    · split at h
      · simp at h
      · rename_i i hi
        cases h; exact ⟨i + 1, pos_lt hi, rfl⟩

theorem deleteLine_spec {ls rs : List Line} {id : Nat} (h : deleteLine ls id = some rs) :
    ∃ d, rs = killAt ls d ∧ isAlive ls d = true ∧ countAlive ls ≠ 1 := by
  unfold deleteLine at h
  split at h
  · simp at h
  · rename_i d _
    split at h
    · simp at h
    · split at h
      · simp at h
      · cases h; exact ⟨d, rfl, by simp_all, by assumption⟩

/-! ### Insertion keeps the grid valid -/

theorem span_insertAt {ls : List Line} {k : Nat} {e : Line} {x y a b : Nat} (hk : k ≤ ls.length)
    (ha : pos ls x = some a) (hb : pos ls y = some b) (he : e.id ∉ ids ls) :
    span (insertAt ls k e) x y = (sh k a, sh k b) := by
  have hx : e.id ≠ x := fun h => he (h ▸ mem_ids_of_pos ha)
  have hy : e.id ≠ y := fun h => he (h ▸ mem_ids_of_pos hb)
  exact span_of_pos (by rw [pos_insertAt hk hx, ha]; rfl) (by rw [pos_insertAt hk hy, hb]; rfl)

theorem valid_insertRow {g : Grid} {rs : List Line} {id : Nat} {after : Option Nat}
    (hv : Valid g) (h : insertLine g.rows id after = some rs) : Valid { g with rows := rs } := by
  obtain ⟨hid, k, hk, rfl⟩ := insertLine_spec h
  have corner : ∀ m ∈ g.merges, (∃ a, pos g.rows m.top = some a) ∧ ∃ b, pos g.rows m.bottom = some b := by
    intro m hm
    have := hv.mergesOk m hm
    simp only [checkMerge, Bool.and_eq_true] at this
    obtain ⟨a, b, ha, hb, -⟩ := axisOk_spec.mp this.1.1
    exact ⟨⟨a, ha⟩, ⟨b, hb⟩⟩
  have ne : ∀ m ∈ g.merges, id ≠ m.top ∧ id ≠ m.bottom := by
    intro m hm
    obtain ⟨⟨a, ha⟩, ⟨b, hb⟩⟩ := corner m hm
    exact ⟨fun h => hid (h ▸ mem_ids_of_pos ha), fun h => hid (h ▸ mem_ids_of_pos hb)⟩
  refine ⟨nodup_insertAt hv.rowsNodup hid, hv.colsNodup, any_alive_insertAt hv.rowAlive,
    hv.colAlive, ?_, ?_⟩
  · intro m hm
    have hc := hv.mergesOk m hm
    obtain ⟨n1, n2⟩ := ne m hm
    simp only [checkMerge, cellCount, Bool.and_eq_true] at hc ⊢
    rw [axisOk_insertAt hk n1 n2]
    exact ⟨⟨hc.1.1, hc.1.2⟩, decide_eq_true
      (Nat.le_trans (of_decide_eq_true hc.2) (Nat.mul_le_mul_right _ (cnt_insertAt hk n1 n2)))⟩
  · refine hv.disjoint.imp_of_mem ?_
    intro a b ha hb hab
    obtain ⟨⟨a1, ha1⟩, ⟨a2, ha2⟩⟩ := corner a ha
    obtain ⟨⟨b1, hb1⟩, ⟨b2, hb2⟩⟩ := corner b hb
    simp only [overlaps] at hab ⊢
    rw [span_insertAt hk ha1 ha2 hid, span_insertAt hk hb1 hb2 hid, inter_sh]
    rw [span_of_pos ha1 ha2, span_of_pos hb1 hb2] at hab
    exact hab

/-! ### Deletion keeps the grid valid -/

/-- `p` lies inside `q`. -/
def Within (p q : Nat × Nat) : Prop := q.1 ≤ p.1 ∧ p.2 ≤ q.2

theorem inter_mono {p1 p2 q1 q2 : Nat × Nat} (h1 : Within p1 q1) (h2 : Within p2 q2)
    (h : inter p1 p2 = true) : inter q1 q2 = true := by
  simp only [inter, Bool.and_eq_true, decide_eq_true_eq, Within] at *
  omega

theorem normalizeOne_spec {g : Grid} (hr : (ids g.rows).Nodup) (hc : (ids g.cols).Nodup)
    {m n : Merge} (h : normalizeOne g m = some n) :
    checkMerge g n = true ∧ Within (span g.rows n.top n.bottom) (span g.rows m.top m.bottom) ∧
      Within (span g.cols n.left n.right) (span g.cols m.left m.right) := by
  unfold normalizeOne at h
  split at h
  · rename_i t b l r hrow hcol
    split at h
    · rename_i h2
      cases h
      obtain ⟨a1, b1, i1, j1, ha1, hb1, hi1, hj1, h11, h12, h13, h14, hl1, hl2⟩ := aliveSpan_spec hr hrow
      obtain ⟨a2, b2, i2, j2, ha2, hb2, hi2, hj2, h21, h22, h23, h24, hl3, hl4⟩ := aliveSpan_spec hc hcol
      simp only [cellCount] at h2
      have hr0 : cnt g.rows t b ≠ 0 := by intro h0; rw [h0] at h2; simp at h2
      have hc0 : cnt g.cols l r ≠ 0 := by intro h0; rw [h0] at h2; simp at h2
      obtain ⟨x1, y1, hx1, hy1, hxy1⟩ := pos_le_of_cnt hr0
      obtain ⟨x2, y2, hx2, hy2, hxy2⟩ := pos_le_of_cnt hc0
      rw [hi1] at hx1; rw [hj1] at hy1; rw [hi2] at hx2; rw [hj2] at hy2
      cases hx1; cases hy1; cases hx2; cases hy2
      refine ⟨?_, ?_, ?_⟩
      · simp only [checkMerge, cellCount, Bool.and_eq_true]
        exact ⟨⟨axisOk_spec.mpr ⟨_, _, hi1, hj1, hl1, hl2, hxy1⟩,
          axisOk_spec.mpr ⟨_, _, hi2, hj2, hl3, hl4, hxy2⟩⟩, decide_eq_true h2⟩
      · simp only [span_of_pos hi1 hj1, span_of_pos ha1 hb1, Within]; omega
      · simp only [span_of_pos hi2 hj2, span_of_pos ha2 hb2, Within]; omega
    · simp at h
  · simp at h

theorem overlaps_normalizeOne {g : Grid} (hr : (ids g.rows).Nodup) (hc : (ids g.cols).Nodup)
    {m1 m2 n1 n2 : Merge} (h1 : normalizeOne g m1 = some n1) (h2 : normalizeOne g m2 = some n2)
    (h : overlaps g m2 m1 = false) : overlaps g n2 n1 = false := by
  obtain ⟨-, r1, c1⟩ := normalizeOne_spec hr hc h1
  obtain ⟨-, r2, c2⟩ := normalizeOne_spec hr hc h2
  cases ho : overlaps g n2 n1
  · rfl
  · simp only [overlaps, Bool.and_eq_true] at ho h
    rw [inter_mono r2 r1 ho.1, inter_mono c2 c1 ho.2] at h
    simp at h

theorem valid_normalize {g : Grid} (hr : (ids g.rows).Nodup) (hc : (ids g.cols).Nodup)
    (hra : g.rows.any (·.alive) = true) (hca : g.cols.any (·.alive) = true)
    (hd : g.merges.Pairwise (fun a b => overlaps g b a = false)) : Valid (normalize g) := by
  refine ⟨hr, hc, hra, hca, ?_, ?_⟩
  · intro n hn
    obtain ⟨m, -, hm⟩ := List.mem_filterMap.mp hn
    exact (normalizeOne_spec hr hc hm).1
  · show (g.merges.filterMap (normalizeOne g)).Pairwise (fun a b => overlaps g b a = false)
    rw [List.pairwise_filterMap]
    refine hd.imp ?_
    intro m1 m2 h n1 hn1 n2 hn2
    exact overlaps_normalizeOne hr hc hn1 hn2 h

theorem valid_deleteRow {g : Grid} {rs : List Line} {id : Nat}
    (hv : Valid g) (h : deleteLine g.rows id = some rs) :
    Valid (normalize { g with rows := rs }) := by
  obtain ⟨d, rfl, hd, h1⟩ := deleteLine_spec h
  have hids : ids (killAt g.rows d) = ids g.rows := ids_killAt _ _
  apply valid_normalize
  · show (ids (killAt g.rows d)).Nodup; rw [hids]; exact hv.rowsNodup
  · exact hv.colsNodup
  · show (killAt g.rows d).any (·.alive) = true
    have := countAlive_killAt hd
    have h0 := (any_alive_iff _).mp hv.rowAlive
    rw [any_alive_iff]; omega
  · exact hv.colAlive
  · have : overlaps { g with rows := killAt g.rows d } = overlaps g := by
      funext a b; simp only [overlaps, span_eq_of_ids hids]
    rw [this]; exact hv.disjoint

/-! ### Merge and unmerge -/

theorem valid_merge {g : Grid} {m : Merge} (hv : Valid g) (hm : checkMerge g m = true)
    (ho : ∀ o ∈ g.merges, overlaps g m o = false) : Valid { g with merges := g.merges ++ [m] } := by
  refine ⟨hv.rowsNodup, hv.colsNodup, hv.rowAlive, hv.colAlive, ?_, ?_⟩
  · intro n hn
    rcases List.mem_append.mp hn with hn | hn
    · exact hv.mergesOk n hn
    · simp at hn; subst hn; exact hm
  · show (g.merges ++ [m]).Pairwise (fun a b => overlaps g b a = false)
    rw [List.pairwise_append]
    refine ⟨hv.disjoint, by simp, ?_⟩
    intro a ha b hb
    simp at hb; subst hb; exact ho a ha

theorem valid_unmerge {g : Grid} {i : Nat} (hv : Valid g) :
    Valid { g with merges := g.merges.eraseIdx i } := by
  refine ⟨hv.rowsNodup, hv.colsNodup, hv.rowAlive, hv.colAlive, ?_, ?_⟩
  · intro n hn
    exact hv.mergesOk n ((List.eraseIdx_sublist _ _).subset hn)
  · exact hv.disjoint.sublist (List.eraseIdx_sublist _ _)

/-! ### Columns are rows of the transposed grid -/

def Merge.tr (m : Merge) : Merge := ⟨m.left, m.top, m.right, m.bottom⟩

def Grid.tr (g : Grid) : Grid := ⟨g.cols, g.rows, g.merges.map Merge.tr⟩

@[simp] theorem Merge.tr_tr (m : Merge) : m.tr.tr = m := rfl

@[simp] theorem Grid.tr_tr (g : Grid) : g.tr.tr = g := by
  simp [Grid.tr, List.map_map, Function.comp_def]

theorem cellCount_tr (g : Grid) (m : Merge) : cellCount g.tr m.tr = cellCount g m := by
  simp [cellCount, Grid.tr, Merge.tr, Nat.mul_comm]

theorem checkMerge_tr (g : Grid) (m : Merge) : checkMerge g.tr m.tr = checkMerge g m := by
  simp only [checkMerge, cellCount_tr]
  simp only [Grid.tr, Merge.tr, Bool.and_comm (axisOk g.cols _ _)]

theorem overlaps_tr (g : Grid) (a b : Merge) : overlaps g.tr a.tr b.tr = overlaps g a b := by
  simp only [overlaps, Grid.tr, Merge.tr, Bool.and_comm]

theorem Valid.tr {g : Grid} (hv : Valid g) : Valid g.tr := by
  refine ⟨hv.colsNodup, hv.rowsNodup, hv.colAlive, hv.rowAlive, ?_, ?_⟩
  · intro n hn
    simp only [Grid.tr, List.mem_map] at hn
    obtain ⟨m, hm, rfl⟩ := hn
    rw [checkMerge_tr]; exact hv.mergesOk m hm
  · show (g.merges.map Merge.tr).Pairwise (fun a b => overlaps g.tr b a = false)
    rw [List.pairwise_map]
    refine hv.disjoint.imp ?_
    intro a b h; rw [overlaps_tr]; exact h

theorem normalizeOne_tr (g : Grid) (m : Merge) :
    normalizeOne g.tr m.tr = (normalizeOne g m).map Merge.tr := by
  unfold normalizeOne
  simp only [Grid.tr, Merge.tr]
  cases aliveSpan g.rows m.top m.bottom with
  | none => cases aliveSpan g.cols m.left m.right <;> rfl
  | some p =>
    cases aliveSpan g.cols m.left m.right with
    | none => rfl
    | some q =>
      obtain ⟨t, b⟩ := p
      obtain ⟨l, r⟩ := q
      have := cellCount_tr g ⟨t, l, b, r⟩
      simp only [Grid.tr, Merge.tr] at this
      simp only [this]
      split <;> rfl

theorem normalize_tr (g : Grid) : normalize g.tr = (normalize g).tr := by
  simp only [normalize, Grid.tr, Grid.mk.injEq, true_and]
  rw [List.filterMap_map, List.map_filterMap]
  congr 1
  funext m
  exact normalizeOne_tr g m

theorem apply_insertCol (g : Grid) (id : Nat) (after : Option Nat) :
    apply g (.insertCol id after) = (apply g.tr (.insertRow id after)).map Grid.tr := by
  simp only [apply, Option.map_map]
  congr 1
  funext cs
  simp [Grid.tr, List.map_map, Function.comp_def]

theorem apply_deleteCol (g : Grid) (id : Nat) :
    apply g (.deleteCol id) = (apply g.tr (.deleteRow id)).map Grid.tr := by
  simp only [apply, Option.map_map]
  congr 1
  funext cs
  have e : (⟨cs, g.rows, g.merges.map Merge.tr⟩ : Grid) = Grid.tr ⟨g.rows, cs, g.merges⟩ := rfl
  simp only [Function.comp_apply, Grid.tr]
  rw [e, normalize_tr]
  exact (Grid.tr_tr _).symm

/-! ## Main theorem -/

theorem row_ops_preserve_valid {g g' : Grid} (hv : Valid g) :
    (∀ id after, apply g (.insertRow id after) = some g' → Valid g') ∧
    (∀ id, apply g (.deleteRow id) = some g' → Valid g') := by
  constructor
  · intro id after h
    simp only [apply, Option.map_eq_some_iff] at h
    obtain ⟨rs, hrs, rfl⟩ := h
    exact valid_insertRow hv hrs
  · intro id h
    simp only [apply, Option.map_eq_some_iff] at h
    obtain ⟨rs, hrs, rfl⟩ := h
    exact valid_deleteRow hv hrs

/-- **Every accepted grid operation keeps the grid valid** (`Grid::apply` keeps
`Grid::check_invariants`; proptest `validated_ops_preserve_invariants`). -/
theorem apply_preserves_valid {g g' : Grid} {op : Op} (hv : Valid g) (h : apply g op = some g') :
    Valid g' := by
  cases op with
  | insertRow id after => exact (row_ops_preserve_valid hv).1 id after h
  | deleteRow id => exact (row_ops_preserve_valid hv).2 id h
  | insertCol id after =>
    rw [apply_insertCol, Option.map_eq_some_iff] at h
    obtain ⟨g'', h'', rfl⟩ := h
    have := (row_ops_preserve_valid hv.tr).1 id after h''
    exact this.tr
  | deleteCol id =>
    rw [apply_deleteCol, Option.map_eq_some_iff] at h
    obtain ⟨g'', h'', rfl⟩ := h
    exact ((row_ops_preserve_valid hv.tr).2 id h'').tr
  | merge m =>
    simp only [apply] at h
    split at h
    · rename_i hc
      cases h
      simp only [Bool.and_eq_true, Bool.not_eq_true', List.any_eq_false] at hc
      exact valid_merge hv hc.1 (fun o ho => by simpa using hc.2 o ho)
    · simp at h
  | unmerge t l =>
    simp only [apply] at h
    split at h
    · simp at h
    · cases h; exact valid_unmerge hv

/-- A rejected operation leaves the sequencer's state exactly as it was. -/
theorem apply_rejects_unchanged {g : Grid} {op : Op} (h : apply g op = none) :
    Sequencer.step apply g op = g := by
  simp [Sequencer.step, h]

/-- An accepted operation's result is the sequencer's new state. -/
theorem apply_accepts {g g' : Grid} {op : Op} (h : apply g op = some g') :
    Sequencer.step apply g op = g' := by
  simp [Sequencer.step, h]

/-- **Sequenced grid logs stay valid**: whatever grid operations arrive, in
whatever order, the sequencer's grid satisfies `check_invariants`. Instance of
`Sequencer.sequenced_preserves`. -/
theorem grid_sequenced_valid {g : Grid} (hv : Valid g) (ops : List Op) :
    Valid (ops.foldl (Sequencer.step apply) g) :=
  Sequencer.sequenced_preserves apply Valid (fun _ _ _ hs h => apply_preserves_valid hs h) ops g hv

/-- Every replica that folds the same log from the same valid snapshot holds the
same valid grid. -/
theorem grid_replicas_valid {g : Grid} (hv : Valid g) (l₁ l₂ : List Op) :
    Valid (l₂.foldl (Sequencer.step apply) (l₁.foldl (Sequencer.step apply) g)) := by
  rw [← Sequencer.replicas_agree]; exact grid_sequenced_valid hv _

/-! ## An executable checker equivalent to `Valid` -/

/-- The duplicate loop of `check_invariants`: no line repeats an identifier seen
before it. -/
def noDupIds (seen : List Nat) : List Line → Bool
  | [] => true
  | l :: ls => !(seen.contains l.id) && noDupIds (seen ++ [l.id]) ls

/-- The merge loop of `check_invariants`: each merge passes `check_merge` and
overlaps none of the earlier ones. -/
def checkMerges (g : Grid) (prev : List Merge) : List Merge → Bool
  | [] => true
  | m :: ms => checkMerge g m && !(prev.any (overlaps g m)) && checkMerges g (prev ++ [m]) ms

/-- `Grid::check_invariants` (`Ok` ↔ `true`). -/
def check (g : Grid) : Bool :=
  noDupIds [] g.rows && noDupIds [] g.cols && g.rows.any (·.alive) && g.cols.any (·.alive) &&
    checkMerges g [] g.merges

theorem noDupIds_iff (seen : List Nat) (ls : List Line) :
    noDupIds seen ls = true ↔ (∀ x ∈ ids ls, x ∉ seen) ∧ (ids ls).Nodup := by
  induction ls generalizing seen with
  | nil => simp [noDupIds, ids]
  | cons l ls ih =>
    simp only [noDupIds, Bool.and_eq_true, Bool.not_eq_true', ih, ids, List.map_cons,
      List.mem_cons, List.nodup_cons, List.contains_eq_mem, decide_eq_false_iff_not] at *
    constructor
    · rintro ⟨h1, h2, h3⟩
      refine ⟨?_, ?_, h3⟩
      · rintro x (rfl | hx)
        · exact h1
        · intro hs; exact h2 x hx (List.mem_append_left _ hs)
      · intro hm; exact h2 _ hm (List.mem_append_right _ (List.mem_singleton_self _))
    · rintro ⟨h1, h2, h3⟩
      refine ⟨h1 _ (Or.inl rfl), ?_, h3⟩
      intro x hx hs
      rcases List.mem_append.mp hs with hs | hs
      · exact h1 x (Or.inr hx) hs
      · simp at hs; subst hs; exact h2 hx

theorem checkMerges_iff (g : Grid) (prev ms : List Merge) :
    checkMerges g prev ms = true ↔
      (∀ m ∈ ms, checkMerge g m = true) ∧ (∀ m ∈ ms, ∀ o ∈ prev, overlaps g m o = false) ∧
        ms.Pairwise (fun a b => overlaps g b a = false) := by
  induction ms generalizing prev with
  | nil => simp [checkMerges]
  | cons m ms ih =>
    simp only [checkMerges, Bool.and_eq_true, Bool.not_eq_true', List.any_eq_false, ih,
      List.mem_cons, List.pairwise_cons, forall_eq_or_imp, List.mem_append]
    constructor
    · rintro ⟨⟨h1, h2⟩, h3, h4, h5⟩
      refine ⟨⟨h1, h3⟩, ⟨fun o ho => by simpa using h2 o ho, fun x hx o ho => h4 x hx o (Or.inl ho)⟩,
        fun x hx => h4 x hx m (Or.inr (Or.inl rfl)), h5⟩
    · rintro ⟨⟨h1, h3⟩, ⟨h2, h4⟩, h6, h5⟩
      refine ⟨⟨h1, fun o ho => by simpa using h2 o ho⟩, h3, ?_, h5⟩
      rintro x hx o (ho | rfl | ho)
      · exact h4 x hx o ho
      · exact h6 x hx
      · simp at ho

/-- **The executable checker decides `Valid`.** -/
theorem check_iff_valid (g : Grid) : check g = true ↔ Valid g := by
  simp only [check, Bool.and_eq_true, noDupIds_iff, checkMerges_iff]
  constructor
  · rintro ⟨⟨⟨⟨⟨-, h1⟩, ⟨-, h2⟩⟩, h3⟩, h4⟩, h5, -, h6⟩
    exact ⟨h1, h2, h3, h4, h5, h6⟩
  · rintro ⟨h1, h2, h3, h4, h5, h6⟩
    exact ⟨⟨⟨⟨⟨by simp, h1⟩, ⟨by simp, h2⟩⟩, h3⟩, h4⟩, h5, by simp, h6⟩

instance (g : Grid) : Decidable (Valid g) := decidable_of_iff _ (check_iff_valid g)

/-! ## Rows and columns through merged regions

What Word and Excel do when a row or column goes through a merged cell, stated
on the cells a merge covers: a line inserted strictly inside a merge joins it
and one inserted at its edge does not (`insertRow_covers`, `insertCol_covers`);
deleting a line takes it out of every merge, so a deleted edge moves inward to
the next alive line, and a merge left with fewer than two cells is dropped
(`deleteRow_covers`, `deleteCol_covers`). `mergeAt` (`Grid::merge_at`) names
the merge covering an alive cell (`mergeAt_spec`). The grid already behaved
this way — merges are named by their corners, so lines between the corners
belong to them — and these theorems pin the behaviour down; `grid-oracle`
prints `mergeAt` of every alive cell so the Rust query is compared too. -/

/-- Line `z` lies between lines `x` and `y`, inclusive, by position — alive or
not. -/
def between (ls : List Line) (x y z : Nat) : Bool :=
  match pos ls x, pos ls y, pos ls z with
  | some a, some b, some i => decide (a ≤ i) && decide (i ≤ b)
  | _, _, _ => false

/-- Merge `m` spans cell (`r`, `c`): its row lies between `m`'s top and bottom
and its column between `m`'s left and right. -/
def covers (g : Grid) (m : Merge) (r c : Nat) : Bool :=
  between g.rows m.top m.bottom r && between g.cols m.left m.right c

/-- Row `r` and column `c` both exist and are alive. -/
def aliveCell (g : Grid) (r c : Nat) : Bool :=
  (alivePos g.rows r).isSome && (alivePos g.cols c).isSome

/-- `Grid::merge_at`: the merge covering an alive cell. -/
def mergeAt (g : Grid) (r c : Nat) : Option Merge :=
  if aliveCell g r c then g.merges.find? (covers g · r c) else none

/-- The alive cells of `m`: what the merge shows as one cell. -/
def Holds (g : Grid) (m : Merge) (r c : Nat) : Prop :=
  covers g m r c = true ∧ aliveCell g r c = true

theorem between_spec {ls : List Line} {x y z : Nat} :
    between ls x y z = true ↔ ∃ a b i, pos ls x = some a ∧ pos ls y = some b ∧
      pos ls z = some i ∧ a ≤ i ∧ i ≤ b := by
  unfold between
  cases pos ls x <;> cases pos ls y <;> cases pos ls z <;> simp

/-- Two merges covering one cell overlap. -/
theorem overlaps_of_covers {g : Grid} {m n : Merge} {r c : Nat}
    (hm : covers g m r c = true) (hn : covers g n r c = true) : overlaps g m n = true := by
  simp only [covers, Bool.and_eq_true] at hm hn
  obtain ⟨⟨a1, b1, i1, ha1, hb1, hi1, h11, h12⟩, ⟨a2, b2, i2, ha2, hb2, hi2, h21, h22⟩⟩ :=
    And.intro (between_spec.mp hm.1) (between_spec.mp hm.2)
  obtain ⟨⟨a3, b3, i3, ha3, hb3, hi3, h31, h32⟩, ⟨a4, b4, i4, ha4, hb4, hi4, h41, h42⟩⟩ :=
    And.intro (between_spec.mp hn.1) (between_spec.mp hn.2)
  rw [hi1] at hi3; rw [hi2] at hi4; cases hi3; cases hi4
  simp only [overlaps, span_of_pos ha1 hb1, span_of_pos ha2 hb2, span_of_pos ha3 hb3,
    span_of_pos ha4 hb4, inter, Bool.and_eq_true, decide_eq_true_eq]
  omega

theorem find?_covers {g : Grid} {r c : Nat} :
    ∀ {ms : List Merge}, ms.Pairwise (fun a b => overlaps g b a = false) →
      ∀ {m}, m ∈ ms → covers g m r c = true → ms.find? (covers g · r c) = some m
  | [], _, _, hm, _ => by simp at hm
  | x :: xs, hp, m, hm, hc => by
    rw [List.pairwise_cons] at hp
    by_cases hx : covers g x r c = true
    · rw [List.find?_cons_of_pos (p := (covers g · r c)) hx]
      rcases List.mem_cons.mp hm with rfl | hm
      · rfl
      · have := hp.1 m hm
        rw [overlaps_of_covers hc hx] at this
        simp at this
    · rw [List.find?_cons_of_neg (p := (covers g · r c)) hx]
      rcases List.mem_cons.mp hm with rfl | hm
      · exact absurd hc hx
      · exact find?_covers hp.2 hm hc

/-- **`merge_at` names the one merge covering an alive cell.** In a valid
grid at most one does, since merges do not overlap. -/
theorem mergeAt_spec {g : Grid} (hv : Valid g) (r c : Nat) (m : Merge) :
    mergeAt g r c = some m ↔ m ∈ g.merges ∧ Holds g m r c := by
  unfold mergeAt Holds
  split
  · rename_i ha
    constructor
    · intro h
      exact ⟨List.mem_of_find?_eq_some h, List.find?_some (p := (covers g · r c)) h, ha⟩
    · rintro ⟨hm, hc, -⟩
      exact find?_covers hv.disjoint hm hc
  · rename_i ha
    simp only [reduceCtorEq, false_iff, not_and]
    intro _ _ h; exact absurd h ha

/-! ### Insertion -/

theorem insertLine_at {ls rs : List Line} {x : Nat} {after : Option Nat}
    (h : insertLine ls x after = some rs) :
    x ∉ ids ls ∧ ((after = none ∧ rs = insertAt ls 0 ⟨x, true⟩) ∨
      ∃ a q, after = some a ∧ pos ls a = some q ∧ rs = insertAt ls (q + 1) ⟨x, true⟩) := by
  refine ⟨(insertLine_spec h).1, ?_⟩
  unfold insertLine at h
  split at h
  · simp at h
  · split at h
    · cases h; exact Or.inl ⟨rfl, rfl⟩
    · rename_i a
      split at h
      · simp at h
      · rename_i q hq
        cases h; exact Or.inr ⟨a, q, rfl, hq, rfl⟩

theorem pos_insertAt_self {ls : List Line} {k : Nat} {e : Line} (hk : k ≤ ls.length)
    (he : e.id ∉ ids ls) : pos (insertAt ls k e) e.id = some k := by
  induction ls generalizing k with
  | nil => simp at hk; subst hk; simp [insertAt_zero, pos]
  | cons l ls ih =>
    cases k with
    | zero => simp [insertAt_zero, pos]
    | succ k =>
      rw [insertAt_succ]
      simp only [ids, List.map_cons, List.mem_cons, not_or] at he
      simp at hk
      simp only [pos, Ne.symm he.1, ite_false, ih (by omega) he.2]
      rfl

theorem between_insertAt_old {ls : List Line} {k : Nat} {e : Line} {x y z : Nat}
    (hk : k ≤ ls.length) (hx : e.id ≠ x) (hy : e.id ≠ y) (hz : e.id ≠ z) :
    between (insertAt ls k e) x y z = between ls x y z := by
  unfold between
  rw [pos_insertAt hk hx, pos_insertAt hk hy, pos_insertAt hk hz]
  cases pos ls x <;> cases pos ls y <;> cases pos ls z <;> simp [sh_le_sh]

theorem between_insertAt_new {ls : List Line} {k : Nat} {e : Line} {x y : Nat}
    (hk : k ≤ ls.length) (he : e.id ∉ ids ls) (hx : e.id ≠ x) (hy : e.id ≠ y) :
    between (insertAt ls k e) x y e.id = true ↔
      ∃ i j, pos ls x = some i ∧ pos ls y = some j ∧ i < k ∧ k ≤ j := by
  unfold between
  rw [pos_insertAt_self hk he, pos_insertAt hk hx, pos_insertAt hk hy]
  cases pos ls x <;> cases pos ls y <;> simp [sh] <;> split <;> split <;> omega

/-- The corners of a merge in a valid grid exist (and are alive). -/
theorem corners {g : Grid} (hv : Valid g) {m : Merge} (hm : m ∈ g.merges) :
    (∃ a b, pos g.rows m.top = some a ∧ pos g.rows m.bottom = some b ∧ a ≤ b) ∧
    (∃ a b, pos g.cols m.left = some a ∧ pos g.cols m.right = some b ∧ a ≤ b) := by
  have := hv.mergesOk m hm
  simp only [checkMerge, Bool.and_eq_true] at this
  obtain ⟨a, b, ha, hb, -, -, hab⟩ := axisOk_spec.mp this.1.1
  obtain ⟨c, d, hc, hd, -, -, hcd⟩ := axisOk_spec.mp this.1.2
  exact ⟨⟨a, b, ha, hb, hab⟩, ⟨c, d, hc, hd, hcd⟩⟩

/-- **A row inserted strictly inside a merge joins it; one inserted at its
edge does not** — Word's and Excel's rule. Every merge keeps its corners and
the columns are untouched; every other row is inside or outside each merge as
before; and the new row is inside merge `m` exactly when it was inserted
after one of `m`'s rows other than its bottom one (a deleted one too). -/
theorem insertRow_covers {g g' : Grid} {x : Nat} {after : Option Nat} (hv : Valid g)
    (h : apply g (.insertRow x after) = some g') :
    g'.merges = g.merges ∧ g'.cols = g.cols ∧
    (∀ m ∈ g.merges, ∀ y, y ≠ x →
      between g'.rows m.top m.bottom y = between g.rows m.top m.bottom y) ∧
    (∀ m ∈ g.merges, between g'.rows m.top m.bottom x = true ↔
      ∃ a i j q, after = some a ∧ pos g.rows m.top = some i ∧ pos g.rows m.bottom = some j ∧
        pos g.rows a = some q ∧ i ≤ q ∧ q < j) := by
  simp only [apply, Option.map_eq_some_iff] at h
  obtain ⟨rs, hrs, rfl⟩ := h
  obtain ⟨hid, hk⟩ := insertLine_at hrs
  have ne : ∀ m ∈ g.merges, x ≠ m.top ∧ x ≠ m.bottom := by
    intro m hm
    obtain ⟨⟨a, b, ha, hb, -⟩, -⟩ := corners hv hm
    exact ⟨fun e => hid (e ▸ mem_ids_of_pos ha), fun e => hid (e ▸ mem_ids_of_pos hb)⟩
  refine ⟨rfl, rfl, ?_, ?_⟩
  · intro m hm y hy
    obtain ⟨n1, n2⟩ := ne m hm
    rcases hk with ⟨-, rfl⟩ | ⟨a, q, -, hq, rfl⟩
    · exact between_insertAt_old (Nat.zero_le _) n1 n2 (Ne.symm hy)
    · exact between_insertAt_old (pos_lt hq) n1 n2 (Ne.symm hy)
  · intro m hm
    obtain ⟨n1, n2⟩ := ne m hm
    rcases hk with ⟨rfl, rfl⟩ | ⟨a, q, rfl, hq, rfl⟩
    · show between (insertAt g.rows 0 ⟨x, true⟩) m.top m.bottom (Line.id ⟨x, true⟩) = true ↔ _
      rw [between_insertAt_new (Nat.zero_le _) hid n1 n2]
      simp
    · show between (insertAt g.rows (q + 1) ⟨x, true⟩) m.top m.bottom (Line.id ⟨x, true⟩) = true ↔ _
      rw [between_insertAt_new (pos_lt hq) hid n1 n2]
      constructor
      · rintro ⟨i, j, hi, hj, h1, h2⟩
        exact ⟨a, i, j, q, rfl, hi, hj, hq, by omega, by omega⟩
      · rintro ⟨a', i, j, q', ha', hi, hj, hq', h1, h2⟩
        cases ha'; rw [hq] at hq'; cases hq'
        exact ⟨i, j, hi, hj, by omega, by omega⟩

/-! ### Deletion -/

theorem head_filter_first {l : List Line} {f e : Line} {i : Nat}
    (hf : (l.filter (·.alive)).head? = some f) (hi : l[i]? = some e) (he : e.alive = true) :
    ∃ k, k ≤ i ∧ l[k]? = some f := by
  induction l generalizing i with
  | nil => simp at hi
  | cons x xs ih =>
    by_cases hx : x.alive = true
    · rw [List.filter_cons_of_pos hx] at hf; simp at hf; subst hf; exact ⟨0, Nat.zero_le _, rfl⟩
    · rw [List.filter_cons_of_neg hx] at hf
      cases i with
      | zero => simp at hi; subst hi; exact absurd he hx
      | succ i =>
        simp at hi
        obtain ⟨k, hk, hk'⟩ := ih hf hi
        exact ⟨k + 1, by omega, by simpa using hk'⟩

theorem getLast_filter_last {l : List Line} {v e : Line} {i : Nat}
    (hv : (l.filter (·.alive)).getLast? = some v) (hi : l[i]? = some e) (he : e.alive = true) :
    ∃ k, i ≤ k ∧ l[k]? = some v := by
  rw [List.getLast?_eq_head?_reverse, ← List.filter_reverse] at hv
  have hil : i < l.length := by
    rcases Nat.lt_or_ge i l.length with h | h
    · exact h
    · rw [List.getElem?_eq_none h] at hi; simp at hi
  have hr : l.reverse[l.length - 1 - i]? = some e := by
    rw [List.getElem?_reverse (by omega)]
    have : l.length - 1 - (l.length - 1 - i) = i := by omega
    rw [this, hi]
  obtain ⟨k, hk, hk'⟩ := head_filter_first hv hr he
  have hkl : k < l.length := by
    rcases Nat.lt_or_ge k l.length with h | h
    · exact h
    · rw [List.getElem?_eq_none (by simpa using h)] at hk'; simp at hk'
  rw [List.getElem?_reverse hkl] at hk'
  exact ⟨l.length - 1 - k, by omega, hk'⟩

theorem getElem?_slice {ls : List Line} {a b k : Nat} (hk : a + k ≤ b) :
    (slice ls a b)[k]? = ls[a + k]? := by
  simp only [slice, List.getElem?_drop, List.getElem?_take]
  split <;> first | rfl | omega

theorem getElem?_slice_some {ls : List Line} {a b k : Nat} {l : Line}
    (h : (slice ls a b)[k]? = some l) : a + k ≤ b ∧ ls[a + k]? = some l := by
  simp only [slice, List.getElem?_drop, List.getElem?_take] at h
  split at h
  · exact ⟨by omega, h⟩
  · simp at h

/-- The corners `aliveSpan` finds enclose every alive line between the old
ones: the first and the last alive line. -/
theorem aliveSpan_bounds {ls : List Line} (hn : (ids ls).Nodup) {x y u v a b p : Nat}
    (h : aliveSpan ls x y = some (u, v)) (ha : pos ls x = some a) (hb : pos ls y = some b)
    (hp1 : a ≤ p) (hp2 : p ≤ b) (hal : isAlive ls p = true) :
    ∃ i j, pos ls u = some i ∧ pos ls v = some j ∧ i ≤ p ∧ p ≤ j := by
  unfold aliveSpan at h
  rw [ha, hb] at h
  simp only at h
  split at h
  · simp at h
  · split at h
    · rename_i f l hf hl
      simp only [Option.some.injEq, Prod.mk.injEq] at h
      obtain ⟨rfl, rfl⟩ := h
      unfold isAlive at hal
      split at hal
      · rename_i e he
        have hs : (slice ls a b)[p - a]? = some e := by
          rw [getElem?_slice (by omega)]; rw [show a + (p - a) = p by omega]; exact he
        obtain ⟨k, hk, hk'⟩ := head_filter_first hf hs hal
        obtain ⟨k', hk2, hk2'⟩ := getLast_filter_last hl hs hal
        obtain ⟨-, g1⟩ := getElem?_slice_some hk'
        obtain ⟨-, g2⟩ := getElem?_slice_some hk2'
        exact ⟨a + k, a + k', pos_of_get hn g1, pos_of_get hn g2, by omega, by omega⟩
      · simp at hal
    · simp at h

/-- On alive lines, the corners `aliveSpan` finds span what the old ones did. -/
theorem aliveSpan_between {ls : List Line} (hn : (ids ls).Nodup) {x y u v z : Nat}
    (h : aliveSpan ls x y = some (u, v)) (hz : (alivePos ls z).isSome = true) :
    between ls u v z = between ls x y z := by
  obtain ⟨a, b, i, j, ha, hb, hi, hj, h1, h2, h3, h4, -, -⟩ := aliveSpan_spec hn h
  unfold alivePos at hz
  split at hz
  · rename_i p hp
    split at hz
    · rename_i hal
      unfold between
      rw [ha, hb, hi, hj, hp]
      by_cases hin : a ≤ p ∧ p ≤ b
      · obtain ⟨i', j', hi', hj', h5, h6⟩ := aliveSpan_bounds hn h ha hb hin.1 hin.2 hal
        rw [hi] at hi'; rw [hj] at hj'; cases hi'; cases hj'
        simp [h5, h6, hin.1, hin.2]
      · by_cases h7 : i ≤ p <;> by_cases h8 : p ≤ j <;> simp [h7, h8] <;> omega
    · simp at hz
  · simp at hz

theorem countAlive_shrink {ls : List Line} {a b i j : Nat} (hai : a ≤ i) (hij : i ≤ j)
    (hjb : j ≤ b)
    (hout : ∀ p, a ≤ p → p ≤ b → isAlive ls p = true → i ≤ p ∧ p ≤ j) :
    countAlive (slice ls i j) = countAlive (slice ls a b) := by
  let L := ls.take (b + 1)
  have hL : ∀ t, t ≤ b → L[t]? = ls[t]? := by
    intro t ht; simp only [L, List.getElem?_take]; split <;> first | rfl | omega
  have e1 : slice ls a b = (L.drop a).take (i - a) ++ L.drop i := by
    show L.drop a = _
    conv => lhs; rw [← List.take_append_drop (i - a) (L.drop a)]
    rw [List.drop_drop, show a + (i - a) = i by omega]
  have e2 : L.drop i = slice ls i j ++ L.drop (j + 1) := by
    have hs : slice ls i j = (L.drop i).take (j + 1 - i) := by
      apply List.ext_getElem?
      intro t
      simp only [slice, L, List.getElem?_take, List.getElem?_drop]
      repeat' split
      all_goals first | rfl | omega
    rw [hs]
    conv => lhs; rw [← List.take_append_drop (j + 1 - i) (L.drop i)]
    rw [List.drop_drop, show i + (j + 1 - i) = j + 1 by omega]
  have z1 : countAlive ((L.drop a).take (i - a)) = 0 := by
    simp only [countAlive, List.countP_eq_zero]
    intro e he hal
    obtain ⟨t, ht⟩ := List.mem_iff_getElem?.mp he
    simp only [List.getElem?_take, List.getElem?_drop] at ht
    split at ht
    · rename_i htl
      rw [hL _ (by omega)] at ht
      have := hout (a + t) (by omega) (by omega) (by simp [isAlive, ht, hal])
      omega
    · simp at ht
  have z2 : countAlive (L.drop (j + 1)) = 0 := by
    simp only [countAlive, List.countP_eq_zero]
    intro e he hal
    obtain ⟨t, ht⟩ := List.mem_iff_getElem?.mp he
    simp only [List.getElem?_drop] at ht
    have htb : j + 1 + t ≤ b := by
      simp only [L, List.getElem?_take] at ht
      split at ht
      · omega
      · simp at ht
    rw [hL _ htb] at ht
    have := hout (j + 1 + t) (by omega) htb (by simp [isAlive, ht, hal])
    omega
  rw [e1, e2]
  simp only [countAlive, List.countP_append] at z1 z2 ⊢
  omega

theorem filter_eq_nil_of_head? {l : List Line} (h : (l.filter (·.alive)).head? = none) :
    countAlive l = 0 := by
  rw [List.head?_eq_none_iff] at h
  simp [countAlive, List.countP_eq_length_filter, h]

theorem cnt_aliveSpan {ls : List Line} (hn : (ids ls).Nodup) {x y u v : Nat}
    (h : aliveSpan ls x y = some (u, v)) : cnt ls u v = cnt ls x y := by
  obtain ⟨a, b, i, j, ha, hb, hi, hj, h1, h2, h3, h4, hli, -⟩ := aliveSpan_spec hn h
  have hij : i ≤ j := by
    obtain ⟨i', j', hi', hj', h5, h6⟩ := aliveSpan_bounds hn h ha hb h1 h2 hli
    rw [hi] at hi'; rw [hj] at hj'; cases hi'; cases hj'; omega
  unfold cnt
  rw [ha, hb, hi, hj]
  simp only [show i ≤ j from hij, show a ≤ b by omega, ite_true]
  exact countAlive_shrink h1 hij h4 (fun p hp1 hp2 hal => by
    obtain ⟨i', j', hi', hj', h5, h6⟩ := aliveSpan_bounds hn h ha hb hp1 hp2 hal
    rw [hi] at hi'; rw [hj] at hj'; cases hi'; cases hj'; exact ⟨h5, h6⟩)

theorem cnt_of_aliveSpan_none {ls : List Line} {x y a b : Nat} (ha : pos ls x = some a)
    (hb : pos ls y = some b) (hab : a ≤ b) (h : aliveSpan ls x y = none) : cnt ls x y = 0 := by
  unfold aliveSpan at h
  rw [ha, hb] at h
  simp only [show ¬ b < a by omega, ite_false] at h
  unfold cnt
  rw [ha, hb]
  simp only [hab, ite_true]
  cases hf : ((slice ls a b).filter (·.alive)).head? with
  | none => exact filter_eq_nil_of_head? hf
  | some f =>
    rw [hf] at h
    have : ((slice ls a b).filter (·.alive)) ≠ [] := by intro e; rw [e] at hf; simp at hf
    cases hl : ((slice ls a b).filter (·.alive)).getLast? with
    | none => rw [List.getLast?_eq_none_iff] at hl; exact absurd hl this
    | some l => rw [hl] at h; simp at h

/-- Normalizing a merge keeps exactly the alive cells it had. -/
theorem normalizeOne_holds {g : Grid} (hr : (ids g.rows).Nodup) (hc : (ids g.cols).Nodup)
    {m n : Merge} (h : normalizeOne g m = some n) (r c : Nat) :
    Holds g n r c ↔ Holds g m r c := by
  unfold normalizeOne at h
  split at h
  · rename_i t b l rt hrow hcol
    split at h
    · cases h
      unfold Holds covers aliveCell
      by_cases ha : (alivePos g.rows r).isSome = true ∧ (alivePos g.cols c).isSome = true
      · rw [aliveSpan_between hr hrow ha.1, aliveSpan_between hc hcol ha.2]
      · simp only [Bool.and_eq_true]
        constructor <;> rintro ⟨-, h1, h2⟩ <;> exact absurd ⟨h1, h2⟩ ha
    · simp at h
  · simp at h

/-- A merge is dropped exactly when fewer than two cells are left between its
corners. -/
theorem normalizeOne_none {g : Grid} (hr : (ids g.rows).Nodup) (hc : (ids g.cols).Nodup)
    {m : Merge}
    (hrow : ∃ a b, pos g.rows m.top = some a ∧ pos g.rows m.bottom = some b ∧ a ≤ b)
    (hcol : ∃ a b, pos g.cols m.left = some a ∧ pos g.cols m.right = some b ∧ a ≤ b) :
    normalizeOne g m = none ↔ cellCount g m < 2 := by
  obtain ⟨a, b, ha, hb, hab⟩ := hrow
  obtain ⟨a', b', ha', hb', hab'⟩ := hcol
  unfold normalizeOne
  cases h1 : aliveSpan g.rows m.top m.bottom with
  | none => simp [cellCount, cnt_of_aliveSpan_none ha hb hab h1]
  | some p =>
    cases h2 : aliveSpan g.cols m.left m.right with
    | none => simp [cellCount, cnt_of_aliveSpan_none ha' hb' hab' h2]
    | some q =>
      obtain ⟨t, bt⟩ := p
      obtain ⟨l, rt⟩ := q
      simp only [cellCount] at *
      simp only [cnt_aliveSpan hr h1, cnt_aliveSpan hc h2]
      split <;> simp <;> omega

theorem deleteLine_at {ls rs : List Line} {x : Nat} (h : deleteLine ls x = some rs) :
    ∃ i, pos ls x = some i ∧ isAlive ls i = true ∧ rs = killAt ls i := by
  unfold deleteLine at h
  split at h
  · simp at h
  · rename_i i hi
    split at h
    · simp at h
    · split at h
      · simp at h
      · cases h; exact ⟨i, hi, by simp_all, rfl⟩

theorem getElem?_killAt (ls : List Line) (i p : Nat) :
    (killAt ls i)[p]? = (ls[p]?).map (fun l => if p = i then { l with alive := false } else l) := by
  induction ls generalizing i p with
  | nil => simp [killAt]
  | cons l ls ih =>
    cases i <;> cases p <;> simp [killAt, ih]

theorem isAlive_killAt (ls : List Line) (i p : Nat) :
    isAlive (killAt ls i) p = (isAlive ls p && decide (p ≠ i)) := by
  unfold isAlive
  rw [getElem?_killAt]
  cases ls[p]? with
  | none => simp
  | some l => by_cases h : p = i <;> simp [h]

theorem alivePos_killAt {ls : List Line} {x i : Nat}
    (hx : pos ls x = some i) (z : Nat) :
    (alivePos (killAt ls i) z).isSome = ((alivePos ls z).isSome && decide (z ≠ x)) := by
  unfold alivePos
  rw [pos_congr (ids_killAt ls i)]
  cases hz : pos ls z with
  | none => simp
  | some p =>
    simp only [isAlive_killAt]
    have : p = i ↔ z = x := by
      constructor
      · intro e; subst e
        obtain ⟨l, hl, hlz⟩ := pos_get hz
        obtain ⟨l', hl', hlx⟩ := pos_get hx
        rw [hl] at hl'; cases hl'; rw [← hlz, ← hlx]
      · intro e; subst e; rw [hx] at hz; cases hz; rfl
    by_cases hp : p = i
    · have := this.mp hp; simp [hp, this]
    · have : ¬ z = x := fun e => hp (this.mpr e); simp [hp, this]

theorem between_killAt (ls : List Line) (i x y z : Nat) :
    between (killAt ls i) x y z = between ls x y z := by
  simp only [between, pos_congr (ids_killAt ls i)]

/-- **Deleting a row takes it out of every merge** — Word's and Excel's rule.
The columns are untouched; each merge is normalized (`normalizeOne`): when it
survives it shows exactly the alive cells it showed minus the deleted row's —
so a deleted top or bottom moves inward to the next alive row — and it is
dropped exactly when fewer than two cells are left between its corners
(`cellCount`, which counts alive rows and columns): deleting the last row it
had, or leaving a single cell. -/
theorem deleteRow_covers {g g' : Grid} {d : Nat} (hv : Valid g)
    (h : apply g (.deleteRow d) = some g') :
    g'.cols = g.cols ∧ g'.merges = g.merges.filterMap (normalizeOne g') ∧
    (∀ m ∈ g.merges, ∀ n, normalizeOne g' m = some n →
      ∀ r c, Holds g' n r c ↔ Holds g m r c ∧ r ≠ d) ∧
    (∀ m ∈ g.merges, normalizeOne g' m = none ↔ cellCount g' m < 2) := by
  simp only [apply, Option.map_eq_some_iff] at h
  obtain ⟨rs, hrs, rfl⟩ := h
  obtain ⟨i, hi, -, rfl⟩ := deleteLine_at hrs
  have hn : (ids (killAt g.rows i)).Nodup := by rw [ids_killAt]; exact hv.rowsNodup
  refine ⟨rfl, rfl, ?_, ?_⟩
  · intro m _ n hn' r c
    have key := normalizeOne_holds (g := { g with rows := killAt g.rows i }) hn hv.colsNodup hn' r c
    refine key.trans ?_
    simp only [Holds, covers, aliveCell, between_killAt, alivePos_killAt hi,
      Bool.and_eq_true, decide_eq_true_eq]
    constructor
    · rintro ⟨hb, ⟨ha, hd⟩, hc⟩; exact ⟨⟨hb, ha, hc⟩, hd⟩
    · rintro ⟨⟨hb, ha, hc⟩, hd⟩; exact ⟨hb, ⟨ha, hd⟩, hc⟩
  · intro m hm
    obtain ⟨⟨a, b, ha, hb, hab⟩, hcol⟩ := corners hv hm
    exact normalizeOne_none (g := { g with rows := killAt g.rows i }) hn hv.colsNodup
      ⟨a, b, by rw [pos_congr (ids_killAt _ _)]; exact ha,
        by rw [pos_congr (ids_killAt _ _)]; exact hb, hab⟩ hcol

/-! ### Columns, through the transposed grid -/

theorem Holds_tr (g : Grid) (m : Merge) (r c : Nat) : Holds g.tr m.tr c r ↔ Holds g m r c := by
  simp only [Holds, covers, aliveCell, Grid.tr, Merge.tr, Bool.and_comm]

/-- **A column inserted strictly inside a merge joins it; one inserted at its
edge does not** (`insertRow_covers` for columns). -/
theorem insertCol_covers {g g' : Grid} {x : Nat} {after : Option Nat} (hv : Valid g)
    (h : apply g (.insertCol x after) = some g') :
    g'.merges = g.merges ∧ g'.rows = g.rows ∧
    (∀ m ∈ g.merges, ∀ y, y ≠ x →
      between g'.cols m.left m.right y = between g.cols m.left m.right y) ∧
    (∀ m ∈ g.merges, between g'.cols m.left m.right x = true ↔
      ∃ a i j q, after = some a ∧ pos g.cols m.left = some i ∧ pos g.cols m.right = some j ∧
        pos g.cols a = some q ∧ i ≤ q ∧ q < j) := by
  rw [apply_insertCol, Option.map_eq_some_iff] at h
  obtain ⟨g'', h'', rfl⟩ := h
  obtain ⟨h1, h2, h3, h4⟩ := insertRow_covers hv.tr h''
  have hm : ∀ m ∈ g.merges, m.tr ∈ g.tr.merges := fun m hm => List.mem_map_of_mem hm
  refine ⟨?_, h2, fun m mm y hy => h3 m.tr (hm m mm) y hy, fun m mm => h4 m.tr (hm m mm)⟩
  show g''.merges.map Merge.tr = g.merges
  rw [h1]; simp [Grid.tr, List.map_map, Function.comp_def]

/-- **Deleting a column takes it out of every merge** (`deleteRow_covers` for
columns): a deleted left or right edge moves inward, and a merge left with
fewer than two cells is dropped. -/
theorem deleteCol_covers {g g' : Grid} {d : Nat} (hv : Valid g)
    (h : apply g (.deleteCol d) = some g') :
    g'.rows = g.rows ∧ g'.merges = g.merges.filterMap (normalizeOne g') ∧
    (∀ m ∈ g.merges, ∀ n, normalizeOne g' m = some n →
      ∀ r c, Holds g' n r c ↔ Holds g m r c ∧ c ≠ d) ∧
    (∀ m ∈ g.merges, normalizeOne g' m = none ↔ cellCount g' m < 2) := by
  rw [apply_deleteCol, Option.map_eq_some_iff] at h
  obtain ⟨g'', h'', rfl⟩ := h
  obtain ⟨h1, h2, h3, h4⟩ := deleteRow_covers hv.tr h''
  have hm : ∀ m ∈ g.merges, m.tr ∈ g.tr.merges := fun m hm => List.mem_map_of_mem hm
  have hn : ∀ m, normalizeOne g''.tr m = (normalizeOne g'' m.tr).map Merge.tr := by
    intro m; have := normalizeOne_tr g'' m.tr; simpa using this
  refine ⟨h1, ?_, ?_, ?_⟩
  · show g''.merges.map Merge.tr = _
    rw [h2]
    have e : g.tr.merges = g.merges.map Merge.tr := rfl
    rw [e, List.filterMap_map, List.map_filterMap]
    congr 1
    funext m
    exact (hn m).symm
  · intro m mm n hmn r c
    rw [hn, Option.map_eq_some_iff] at hmn
    obtain ⟨n', hn', rfl⟩ := hmn
    rw [Holds_tr g'' n' c r, h3 m.tr (hm m mm) n' hn' c r, Holds_tr g m r c]
  · intro m mm
    rw [hn, Option.map_eq_none_iff, h4 m.tr (hm m mm)]
    have := cellCount_tr g'' m.tr
    simp only [Merge.tr_tr] at this
    rw [this]

/-! ## Sanity checks mirroring `crates/jade-collab-core/tests/grid.rs` -/

/-- `grid(rows, cols)` of the Rust tests: rows `1..=rows`, columns `101..=100+cols`. -/
def testGrid (rows cols : Nat) : Grid :=
  Grid.new ((List.range rows).map (· + 1)) ((List.range cols).map (· + 101))

/-- Apply operations in order, failing on the first rejection (`unwrap()`). -/
def run (g : Grid) (ops : List Op) : Option Grid := ops.foldlM apply g

/-- `Grid::rows` / `Grid::columns`: alive identifiers in order. -/
def aliveIds (ls : List Line) : List Nat := (ls.filter (·.alive)).map (·.id)

/-- `inserts_rows_and_columns_after_an_anchor`. -/
theorem sanity_insert_after_anchor :
    ((run (testGrid 2 2) [.insertRow 50 (some 1), .insertRow 51 none, .insertCol 150 (some 102)]).map
      fun g => (aliveIds g.rows, aliveIds g.cols)) = some ([51, 1, 50, 2], [101, 102, 150]) := by
  decide

/-- `deleted_rows_keep_anchoring_concurrent_inserts`. -/
theorem sanity_deleted_row_anchors_insert :
    ((run (testGrid 3 1) [.deleteRow 2, .insertRow 60 (some 2)]).map
      fun g => (aliveIds g.rows, ids g.rows)) = some ([1, 60, 3], [1, 2, 60, 3]) := by
  decide

/-- `refuses_to_delete_the_last_row_or_column`. -/
theorem sanity_last_row_and_column :
    apply (testGrid 1 1) (.deleteRow 1) = none ∧ apply (testGrid 1 1) (.deleteCol 101) = none := by
  decide

/-- `merges_must_not_overlap`. -/
theorem sanity_overlapping_merge_rejected :
    (run (testGrid 3 3) [.merge ⟨1, 101, 2, 102⟩]).bind (apply · (.merge ⟨2, 102, 3, 103⟩)) = none ∧
    ((run (testGrid 3 3) [.merge ⟨1, 101, 2, 102⟩, .merge ⟨3, 101, 3, 103⟩]).map
      (·.merges.length)) = some 2 := by
  decide

/-- `merges_must_be_well_formed`: inverted, single-cell and unknown corners. -/
theorem sanity_malformed_merges_rejected :
    apply (testGrid 3 3) (.merge ⟨2, 101, 1, 102⟩) = none ∧
    apply (testGrid 3 3) (.merge ⟨1, 101, 1, 101⟩) = none ∧
    apply (testGrid 3 3) (.merge ⟨1, 101, 9, 102⟩) = none := by
  decide

/-- `deleting_an_anchor_row_shrinks_the_merge`: deleting the top row moves the
merge's top to the next alive row; deleting its bottom then leaves a single
cell, so the merge disappears. -/
theorem sanity_delete_shrinks_merge :
    ((run (testGrid 3 1) [.merge ⟨1, 101, 3, 101⟩, .deleteRow 1]).map (·.merges)) =
      some [⟨2, 101, 3, 101⟩] ∧
    ((run (testGrid 3 1) [.merge ⟨1, 101, 3, 101⟩, .deleteRow 1, .deleteRow 3]).map
      fun g => (g.merges, check g)) = some ([], true) := by
  decide

/-- `unmerge_removes_the_region_at_its_top_left`. -/
theorem sanity_unmerge :
    (run (testGrid 2 2) [.merge ⟨1, 101, 2, 102⟩]).bind (apply · (.unmerge 2 101)) = none ∧
    ((run (testGrid 2 2) [.merge ⟨1, 101, 2, 102⟩, .unmerge 1 101]).map (·.merges)) = some [] := by
  decide

/-- `naive_union_of_concurrent_merges_breaks_the_invariant`, on the identifier
model: each merge is valid alone, their union is not. -/
theorem sanity_naive_union_invalid :
    check { testGrid 3 3 with merges := [⟨1, 101, 2, 102⟩] } = true ∧
    check { testGrid 3 3 with merges := [⟨2, 102, 3, 103⟩] } = true ∧
    check { testGrid 3 3 with merges := [⟨1, 101, 2, 102⟩, ⟨2, 102, 3, 103⟩] } = false := by
  decide

/-- `a_row_inserted_inside_a_merge_joins_it`: inside joins, below the bottom
and above the top do not. -/
theorem sanity_insert_inside_merge :
    ((run (testGrid 3 3) [.merge ⟨1, 101, 2, 102⟩, .insertRow 50 (some 1), .insertRow 51 (some 2),
      .insertRow 52 none]).map fun g =>
        (mergeAt g 50 101, mergeAt g 50 103, mergeAt g 51 101, mergeAt g 52 101, g.merges)) =
      some (some ⟨1, 101, 2, 102⟩, none, none, none, [⟨1, 101, 2, 102⟩]) := by
  decide

/-- `deleting_a_merge_edge_moves_it_inward`. -/
theorem sanity_delete_edges_move_inward :
    ((run (testGrid 4 4) [.merge ⟨1, 101, 3, 103⟩, .deleteRow 3, .deleteCol 101, .deleteCol 102]).map
      (·.merges)) = some [⟨1, 103, 2, 103⟩] := by
  decide

/-- `a_merge_without_two_cells_disappears`: a 1 × 2 merge losing a column. -/
theorem sanity_single_cell_merge_dropped :
    ((run (testGrid 2 2) [.merge ⟨1, 101, 1, 102⟩, .deleteCol 101]).map (·.merges)) = some [] := by
  decide

theorem testGrid_valid : Valid (testGrid 3 3) := by decide

end JadeCollab.Grid
