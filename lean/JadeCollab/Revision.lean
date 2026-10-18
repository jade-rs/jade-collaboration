/-!
# Tracked changes as attributes of characters

design.md §4 "Tracked changes": a docx character carries the tracked
insertion that made it (until it is accepted) and the *set* of tracked
deletions that named it; removal (a tombstone) is separate. Mirrors
`crates/jade-docx-collab/src/model/tracked.rs`:

* convergent: `trackDelete d as` (`TrackDeleteText`: add `d` to each named
  character's set — removed characters included), `remove as` (`DeleteText`,
  a tombstone) and `insert as r` (`TrackInsertText`, fresh characters made by
  insertion `r`; `InsertText` is the same with no revision);
* sequenced: accepting / rejecting one revision (`AcceptRevisions` /
  `RejectRevisions` name whole revisions).

The theorems: every convergent operation commutes with every operation that
does not decide the revision it creates (the client fast path's condition,
`Reconcile.fast_path_sound`), the deletion sets only grow under the
convergent lane, and replicas that applied the same tracked deletions and
removals in any order hold the same state — so the deletion Word shows (the
set's least revision) is the same everywhere.
-/

namespace JadeCollab.Revision

/-- Characters and revisions are numbered. -/
structure State where
  removed : Nat → Bool
  ins : Nat → Option Nat
  /-- `del a d`: deletion `d` named character `a`. -/
  del : Nat → Nat → Bool

inductive Op where
  | trackDelete (d : Nat) (atoms : List Nat)
  | remove (atoms : List Nat)
  | insert (atoms : List Nat) (r : Option Nat)
  | acceptIns (r : Nat)
  | rejectIns (r : Nat)
  | acceptDel (d : Nat)
  | rejectDel (d : Nat)

def step (s : State) : Op → State
  | .trackDelete d as =>
    { s with del := fun a e => s.del a e || (e == d && as.contains a) }
  | .remove as => { s with removed := fun a => s.removed a || as.contains a }
  | .insert as r => { s with ins := fun a => if as.contains a then r else s.ins a }
  | .acceptIns r => { s with ins := fun a => if s.ins a == some r then none else s.ins a }
  | .rejectIns r => { s with removed := fun a => s.removed a || s.ins a == some r }
  | .acceptDel d => { s with removed := fun a => s.removed a || s.del a d }
  | .rejectDel d => { s with del := fun a e => s.del a e && e != d }

/-- The convergent lane. -/
def Convergent : Op → Prop
  | .trackDelete .. | .remove _ | .insert .. => True
  | _ => False

/-- What a convergent operation needs of an operation concurrent with it:
a decision never names the revision the convergent operation creates (it is
decided only after its author's operation committed), and two insertions
make different characters (fresh ids). -/
def Indep : Op → Op → Prop
  | .trackDelete d _, .acceptDel d' => d ≠ d'
  | .trackDelete d _, .rejectDel d' => d ≠ d'
  | .insert _ r, .acceptIns r' => r ≠ some r'
  | .insert _ r, .rejectIns r' => r ≠ some r'
  | .insert as _, .insert bs _ => ∀ a, a ∈ as → a ∉ bs
  | _, _ => True

/-- An insertion's characters are new: no insertion claims them yet. -/
def Fresh (s : State) : Op → Prop
  | .insert as _ => ∀ a, a ∈ as → s.ins a = none
  | _ => True

theorem State.ext' {s t : State} (h₁ : s.removed = t.removed) (h₂ : s.ins = t.ins)
    (h₃ : s.del = t.del) : s = t := by
  cases s; cases t; simp_all

/-- Tracked deletions commute with each other. -/
theorem trackDelete_comm_trackDelete (s : State) (d d' : Nat) (as bs : List Nat) :
    step (step s (.trackDelete d as)) (.trackDelete d' bs) =
      step (step s (.trackDelete d' bs)) (.trackDelete d as) := by
  refine State.ext' rfl rfl ?_
  funext a e
  simp only [step, Bool.or_assoc]
  rw [Bool.or_comm (e == d && as.contains a)]

/-- A tracked deletion commutes with every operation but a decision on
itself. -/
theorem trackDelete_comm (s : State) (d : Nat) (as : List Nat) (o : Op)
    (h : Indep (.trackDelete d as) o) :
    step (step s (.trackDelete d as)) o = step (step s o) (.trackDelete d as) := by
  cases o with
  | trackDelete d' bs => exact trackDelete_comm_trackDelete s d d' as bs
  | remove bs => rfl
  | insert bs r => rfl
  | acceptIns r => rfl
  | rejectIns r => rfl
  | acceptDel d' =>
    refine State.ext' ?_ rfl rfl
    funext a
    simp only [step]
    have : (d' == d) = false := by simp [Indep] at h; simp [Ne.symm h]
    simp [this]
  | rejectDel d' =>
    refine State.ext' rfl rfl ?_
    funext a e
    simp only [step]
    by_cases he : e = d'
    · subst he
      have : (e == d) = false := by simp [Indep] at h; simp [Ne.symm h]
      simp [this]
    · cases s.del a e <;> simp [he]

/-- A removal (a tombstone) commutes with everything. -/
theorem remove_comm (s : State) (as : List Nat) (o : Op) :
    step (step s (.remove as)) o = step (step s o) (.remove as) := by
  cases o with
  | trackDelete d bs => rfl
  | remove bs =>
    refine State.ext' ?_ rfl rfl
    funext a
    simp only [step, Bool.or_assoc]
    rw [Bool.or_comm (as.contains a)]
  | insert bs r => rfl
  | acceptIns r => rfl
  | rejectIns r =>
    refine State.ext' ?_ rfl rfl
    funext a
    simp only [step, Bool.or_assoc]
    rw [Bool.or_comm (as.contains a)]
  | acceptDel d =>
    refine State.ext' ?_ rfl rfl
    funext a
    simp only [step, Bool.or_assoc]
    rw [Bool.or_comm (as.contains a)]
  | rejectDel d => rfl

/-- An insertion commutes with every operation that does not decide it,
on new characters. -/
theorem insert_comm (s : State) (as : List Nat) (r : Option Nat) (o : Op)
    (h : Indep (.insert as r) o) (fresh : Fresh s (.insert as r)) :
    step (step s (.insert as r)) o = step (step s o) (.insert as r) := by
  cases o with
  | trackDelete d bs => rfl
  | remove bs => rfl
  | insert bs r' =>
    refine State.ext' rfl ?_ rfl
    funext a
    simp only [step]
    by_cases ha : a ∈ as
    · have hb : a ∉ bs := h a ha
      simp [ha, hb]
    · simp [ha]
  | acceptIns r' =>
    refine State.ext' rfl ?_ rfl
    funext a
    simp only [step]
    by_cases ha : a ∈ as
    · simp only [List.contains_iff_mem, ha, ↓reduceIte]
      have : (r == some r') = false := by
        simp only [Indep] at h
        simpa using h
      simp [this]
    · simp [ha]
  | rejectIns r' =>
    refine State.ext' ?_ rfl rfl
    funext a
    simp only [step]
    by_cases ha : a ∈ as
    · simp only [List.contains_iff_mem, ha, ↓reduceIte]
      have : (r == some r') = false := by
        simp only [Indep] at h
        simpa using h
      simp [this, fresh a ha]
    · simp [ha]
  | acceptDel d => rfl
  | rejectDel d => rfl

/-- **The fast path's condition** (design.md §2, `Reconcile.fast_path_sound`):
every convergent operation commutes with every operation independent of it,
sequenced decisions included. -/
theorem convergent_comm (s : State) (c o : Op) (hc : Convergent c) (h : Indep c o)
    (fresh : Fresh s c) : step (step s c) o = step (step s o) c := by
  cases c with
  | trackDelete d as => exact trackDelete_comm s d as o h
  | remove as => exact remove_comm s as o
  | insert as r => exact insert_comm s as r o h fresh
  | acceptIns _ | rejectIns _ | acceptDel _ | rejectDel _ => exact hc.elim

/-- Deletion sets only grow under the convergent lane. -/
theorem convergent_grows (s : State) (c : Op) (hc : Convergent c) (a e : Nat)
    (h : s.del a e = true) : (step s c).del a e = true := by
  cases c with
  | trackDelete d as => simp [step, h]
  | remove _ => exact h
  | insert _ _ => exact h
  | acceptIns _ | rejectIns _ | acceptDel _ | rejectDel _ => exact hc.elim

/-- Operations that commute with each other unconditionally. -/
def Plain : Op → Prop
  | .trackDelete .. | .remove _ => True
  | _ => False

theorem plain_comm (s : State) (x y : Op) (hx : Plain x) (hy : Plain y) :
    step (step s x) y = step (step s y) x := by
  cases x with
  | trackDelete d as =>
    cases y with
    | trackDelete d' bs => exact trackDelete_comm_trackDelete s d d' as bs
    | remove bs => exact (remove_comm s bs (.trackDelete d as)).symm
    | _ => exact hy.elim
  | remove as => exact remove_comm s as y
  | _ => exact hx.elim

/-- Applying a list of operations. -/
def run (s : State) (ops : List Op) : State := ops.foldl step s

theorem run_perm {l₁ l₂ : List Op} (hp : l₁.Perm l₂) (hl : ∀ x, x ∈ l₁ → Plain x) :
    ∀ s, run s l₁ = run s l₂ := by
  induction hp with
  | nil => intro s; rfl
  | cons x _ ih =>
    intro s
    simp only [run, List.foldl_cons] at ih ⊢
    exact ih (fun y hy => hl y (List.mem_cons_of_mem _ hy)) _
  | swap x y l =>
    intro s
    simp only [run, List.foldl_cons]
    rw [plain_comm s y x (hl y List.mem_cons_self)
      (hl x (List.mem_cons_of_mem _ List.mem_cons_self))]
  | trans h₁₂ _ ih₁ ih₂ =>
    intro s
    rw [ih₁ hl, ih₂ (fun y hy => hl y (h₁₂.mem_iff.mpr hy))]

/-- **Convergence of tracked deletions** (`Revision.converges`): replicas
that applied the same tracked deletions and removals, in any order, hold
the same state — and so show every character's deletion attributed to the
same (least) revision. -/
theorem converges {l₁ l₂ : List Op} (s : State) (hp : l₁.Perm l₂)
    (hl : ∀ x, x ∈ l₁ → Plain x) : run s l₁ = run s l₂ :=
  run_perm hp hl s

/-- The attribution Word shows — any function of a character's deletion
set, such as its least member — agrees wherever the state does. -/
theorem shown_converges {l₁ l₂ : List Op} (s : State) (hp : l₁.Perm l₂)
    (hl : ∀ x, x ∈ l₁ → Plain x) (shown : (Nat → Bool) → Option Nat) (a : Nat) :
    shown ((run s l₁).del a) = shown ((run s l₂).del a) := by
  rw [converges s hp hl]

/-- Rejecting a deletion keeps every other deletion of a character: the set
loses exactly that member. -/
theorem rejectDel_keeps_others (s : State) (d a e : Nat) (h : e ≠ d) :
    (step s (.rejectDel d)).del a e = s.del a e := by
  simp [step, h]

end JadeCollab.Revision
