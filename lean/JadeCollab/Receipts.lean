import Std

/-! Retained verdicts are indexed by connection-local submission ticket.
Unknown answers do not establish failure or success; observing another
ticket cannot change a known answer. Rust tests cover receipt routing,
bounded retention, cancellation/timeout boundaries and real socket verdicts. -/
namespace JadeCollab.Receipts

abbrev Answers := Nat → Option Bool

def observe (answers : Answers) (ticket : Nat) (verdict : Bool) : Answers :=
  fun key => if key = ticket then (answers key).or (some verdict) else answers key

theorem known_answer_is_final (answers : Answers) (ticket key : Nat) (old fresh : Bool)
    (h : answers key = some old) : observe answers ticket fresh key = some old := by
  simp [observe, h]

theorem another_ticket_is_unchanged (answers : Answers) (ticket key : Nat) (verdict : Bool)
    (h : key ≠ ticket) : observe answers ticket verdict key = answers key := by
  simp [observe, h]

theorem unknown_ticket_learns_verdict (answers : Answers) (ticket : Nat) (verdict : Bool)
    (h : answers ticket = none) : observe answers ticket verdict ticket = some verdict := by
  simp [observe, h]

def complete (answers : Answers) (tickets : List Nat) : Prop :=
  ∀ ticket ∈ tickets, ∃ verdict, answers ticket = some verdict

def refused (answers : Answers) (tickets : List Nat) : Prop :=
  ∃ ticket ∈ tickets, answers ticket = some false

theorem completed_receipt_stays_complete (answers : Answers) (tickets : List Nat)
    (ticket : Nat) (verdict : Bool) (h : complete answers tickets) :
    complete (observe answers ticket verdict) tickets := by
  intro key member
  obtain ⟨old, known⟩ := h key member
  exact ⟨old, known_answer_is_final answers ticket key old verdict known⟩

theorem observed_refusal_is_final (answers : Answers) (tickets : List Nat)
    (ticket : Nat) (verdict : Bool) (h : refused answers tickets) :
    refused (observe answers ticket verdict) tickets := by
  obtain ⟨key, member, known⟩ := h
  exact ⟨key, member, known_answer_is_final answers ticket key false verdict known⟩

inductive Status where
  | pending | applied | rejected
deriving DecidableEq

def settled (answers : Answers) (tickets : List Nat) : Bool :=
  tickets.all fun ticket => (answers ticket).isSome

def hasRefusal (answers : Answers) (tickets : List Nat) : Bool :=
  tickets.any fun ticket => answers ticket == some false

def report (answers : Answers) (tickets : List Nat) : Status :=
  if hasRefusal answers tickets then .rejected
  else if settled answers tickets then .applied else .pending

theorem applied_requires_every_answer (answers : Answers) (tickets : List Nat)
    (h : report answers tickets = .applied) : settled answers tickets = true := by
  unfold report at h
  split at h
  · contradiction
  · split at h
    · assumption
    · contradiction

theorem rejected_requires_a_refusal (answers : Answers) (tickets : List Nat)
    (h : report answers tickets = .rejected) : hasRefusal answers tickets = true := by
  unfold report at h
  split at h
  · assumption
  · split at h <;> contradiction

theorem unknown_is_pending (first : Nat) (rest : List Nat) :
    report (fun _ => none) (first :: rest) = .pending := by
  simp [report, settled, hasRefusal]

theorem no_operations_is_complete (answers : Answers) : report answers [] = .applied := by
  simp [report, settled, hasRefusal]

/-- FIFO pending tickets are strictly increasing. Once the oldest is after
the receipt's bound, every pending operation belongs to a later prefix. -/
theorem oldest_suffices_for_prefix_wait (first bound : Nat) (rest : List Nat)
    (ordered : List.Pairwise (· < ·) (first :: rest)) :
    bound < first ↔ ∀ ticket ∈ first :: rest, bound < ticket := by
  constructor
  · intro before ticket member
    rcases List.mem_cons.mp member with same | later
    · simpa [same] using before
    · exact Nat.lt_trans before ((List.pairwise_cons.mp ordered).1 ticket later)
  · intro all
    exact all first (by simp)

end JadeCollab.Receipts
