import Std

/-! DOCX locations and revisions use globally addressed flow atoms. New peer
allocations must be disjoint from all retained stories, not just the target
story or visible projection. Rust extracts whole implicit ranges, including
note anchors/tabs, checks exact `(clock, owner)` IDs, and preserves old recorded
facts. This model abstracts native writes, Rust field extraction, bounded
arithmetic and snapshot IO; stateful native Rust tests check those boundaries. -/
namespace JadeCollab.AtomIdentity

structure Id where
  owner : Nat
  clock : Nat
  deriving DecidableEq

def run (owner first count : Nat) : List Id :=
  (List.range count).map (fun offset => ⟨owner, first + offset⟩)

theorem contiguous_runs_have_no_internal_duplicates (owner first count : Nat) :
    (run owner first count).Nodup := by
  unfold run List.Nodup
  apply List.Pairwise.map _ _ List.nodup_range
  intro a b different same
  have clocks := congrArg Id.clock same
  apply different
  simpa using clocks

def fresh (retained allocated : List Id) : Bool :=
  allocated.all (fun id => !retained.contains id)

theorem admission_checks_every_implicit_atom (retained allocated : List Id) :
    fresh retained allocated = true ↔ ∀ id ∈ allocated, id ∉ retained := by
  simp [fresh, List.all_eq_true]

theorem any_reused_atom_refuses_the_whole_run (retained allocated : List Id) (id : Id)
    (old : id ∈ retained) (new : id ∈ allocated) : fresh retained allocated = false := by
  cases h : fresh retained allocated
  · rfl
  · exact False.elim ((admission_checks_every_implicit_atom retained allocated).mp h id new old)

theorem run_contains_exactly_its_owner_and_offsets (owner first count : Nat) (id : Id) :
    id ∈ run owner first count ↔ ∃ offset < count, id = ⟨owner, first + offset⟩ := by
  simp [run, eq_comm]

theorem overlap_in_the_middle_is_refused (retained : List Id) (owner first count offset : Nat)
    (inside : offset < count) (used : Id.mk owner (first + offset) ∈ retained) :
    fresh retained (run owner first count) = false := by
  apply any_reused_atom_refuses_the_whole_run _ _ _ used
  exact (run_contains_exactly_its_owner_and_offsets owner first count _).mpr ⟨offset, inside, rfl⟩

theorem a_different_owner_at_the_same_clock_does_not_collide (a b clock : Nat) (different : a ≠ b) :
    fresh [⟨a,clock⟩] [⟨b,clock⟩] = true := by
  simp [fresh, Id.mk.injEq, Ne.symm different]

theorem unused_old_clock_slots_are_allowed (retained : List Id) (id : Id) (unused : id ∉ retained) :
    fresh retained [id] = true := by simp [fresh, unused]

theorem accepted_atoms_preserve_global_uniqueness (retained allocated : List Id)
    (oldUnique : retained.Nodup) (newUnique : allocated.Nodup)
    (admitted : fresh retained allocated = true) : (retained ++ allocated).Nodup := by
  apply List.nodup_append.mpr
  refine ⟨oldUnique, newUnique, ?_⟩
  intro a old b new same
  subst b
  exact (admission_checks_every_implicit_atom retained allocated).mp admitted a new old

structure Op where
  allocated : List Id
  tracked : Bool := false
  deriving DecidableEq

structure State where
  retained : List Id := []
  visible : List Id := []
  processed : Nat := 0
  logPosition : Nat := 0
  authorRegistered : Bool := false
  deriving DecidableEq

def submit (ticket : Nat) (op : Op) (s : State) : State :=
  if ticket ≤ s.processed then s else
  let advanced := {s with processed := ticket}
  if fresh s.retained op.allocated then
    {advanced with
      retained := s.retained ++ op.allocated,
      visible := s.visible ++ op.allocated,
      logPosition := s.logPosition + 1 + (if op.tracked && !s.authorRegistered then 1 else 0),
      authorRegistered := s.authorRegistered || op.tracked}
  else advanced

theorem duplicate_ticket_changes_nothing (ticket : Nat) (op : Op) (s : State)
    (duplicate : ticket ≤ s.processed) : submit ticket op s = s := by simp [submit, duplicate]

theorem collision_refusal_preserves_model_file_log_and_author (ticket : Nat) (op : Op) (s : State)
    (collision : fresh s.retained op.allocated = false) :
    (submit ticket op s).retained = s.retained ∧
    (submit ticket op s).visible = s.visible ∧
    (submit ticket op s).logPosition = s.logPosition ∧
    (submit ticket op s).authorRegistered = s.authorRegistered := by
  unfold submit
  split <;> simp [collision]

theorem refused_ticket_is_processed (ticket : Nat) (op : Op) (s : State)
    (newTicket : s.processed < ticket) (collision : fresh s.retained op.allocated = false) :
    (submit ticket op s).processed = ticket := by
  simp [submit, Nat.not_le.mpr newTicket, collision]

theorem corrected_payload_cannot_revive_a_refused_ticket (ticket : Nat) (bad corrected : Op) (s : State)
    (newTicket : s.processed < ticket) (collision : fresh s.retained bad.allocated = false) :
    submit ticket corrected (submit ticket bad s) = submit ticket bad s := by
  apply duplicate_ticket_changes_nothing
  rw [refused_ticket_is_processed ticket bad s newTicket collision]
  exact Nat.le_refl ticket

theorem hiding_or_deleting_stories_never_frees_identifiers (s : State) (visible allocated : List Id) :
    fresh {s with visible := visible}.retained allocated = fresh s.retained allocated := rfl

theorem hidden_atoms_still_refuse_new_allocations (s : State) (visible allocated : List Id) (id : Id)
    (used : id ∈ s.retained) (reused : id ∈ allocated) :
    fresh {s with visible := visible}.retained allocated = false :=
  any_reused_atom_refuses_the_whole_run _ _ _ used reused

theorem every_submission_preserves_global_uniqueness (ticket : Nat) (op : Op) (s : State)
    (oldUnique : s.retained.Nodup) (newUnique : op.allocated.Nodup) :
    (submit ticket op s).retained.Nodup := by
  unfold submit
  split
  · exact oldUnique
  · split
    · exact accepted_atoms_preserve_global_uniqueness _ _ oldUnique newUnique ‹_›
    · exact oldUnique

def execute (s : State) : List (Nat × Op) → State
  | [] => s
  | (ticket,op) :: rest => execute (submit ticket op s) rest

theorem arbitrary_stateful_histories_preserve_global_uniqueness (s : State) (steps : List (Nat × Op))
    (oldUnique : s.retained.Nodup) (runsUnique : ∀ step ∈ steps, step.2.allocated.Nodup) :
    (execute s steps).retained.Nodup := by
  induction steps generalizing s with
  | nil => exact oldUnique
  | cons step rest ih =>
    apply ih
    · exact every_submission_preserves_global_uniqueness _ _ _ oldUnique (runsUnique step (by simp))
    · intro next member
      exact runsUnique next (by simp [member])

end JadeCollab.AtomIdentity
