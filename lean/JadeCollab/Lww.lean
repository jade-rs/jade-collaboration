/-!
# Last-writer-wins registers

Mirrors `LwwRegister` in `crates/jade-collab-core/src/lww.rs` (cell values in
the convergent lane, design.md §2/§4). A write is a stamp
`(lamport, replica)` with a value; the register keeps the greatest. The Rust
stamps are unique per write, so comparing `Id` alone suffices there; here the
value is the last tiebreak so that the order is total on whole writes and no
uniqueness assumption is needed.
-/

namespace JadeCollab.Lww

/-- `(lamport, replica, value)`. -/
abbrev Write := Nat × Nat × Nat

/-- `none` = never written. -/
abbrev Reg := Option Write

/-- Strict lexicographic order on writes. -/
def lt (a b : Write) : Bool :=
  decide (a.1 < b.1) ||
    (decide (a.1 = b.1) &&
      (decide (a.2.1 < b.2.1) || (decide (a.2.1 = b.2.1) && decide (a.2.2 < b.2.2))))

/-- The lexicographically greater write. -/
def maxW (a b : Write) : Write :=
  if lt a b then b else a

/-- Join of two registers (`LwwRegister::merge`). -/
def merge : Reg → Reg → Reg
  | none, r => r
  | some a, none => some a
  | some a, some b => some (maxW a b)

/-- Apply one write (`LwwRegister::apply`). -/
def write (r : Reg) (w : Write) : Reg :=
  merge r (some w)

theorem maxW_comm (a b : Write) : maxW a b = maxW b a := by
  obtain ⟨a1, a2, a3⟩ := a
  obtain ⟨b1, b2, b3⟩ := b
  unfold maxW lt
  simp only [Bool.or_eq_true, Bool.and_eq_true, decide_eq_true_eq]
  split <;> split <;> simp only [Prod.mk.injEq] <;> omega

theorem maxW_assoc (a b c : Write) : maxW (maxW a b) c = maxW a (maxW b c) := by
  obtain ⟨a1, a2, a3⟩ := a
  obtain ⟨b1, b2, b3⟩ := b
  obtain ⟨c1, c2, c3⟩ := c
  unfold maxW lt
  simp only [Bool.or_eq_true, Bool.and_eq_true, decide_eq_true_eq]
  split <;> split <;> (try split) <;> (try split) <;> simp_all only [Prod.mk.injEq] <;> omega

theorem maxW_idem (a : Write) : maxW a a = a := by
  unfold maxW
  split <;> rfl

/-- Merge is commutative. -/
theorem merge_comm (r s : Reg) : merge r s = merge s r := by
  cases r <;> cases s <;> simp [merge, maxW_comm]

/-- Merge is associative. -/
theorem merge_assoc (r s t : Reg) : merge (merge r s) t = merge r (merge s t) := by
  cases r <;> cases s <;> cases t <;> simp [merge, maxW_assoc]

/-- Merge is idempotent. -/
theorem merge_idem (r : Reg) : merge r r = r := by
  cases r <;> simp [merge, maxW_idem]

/-- Concurrent writes commute (backs `LwwRegister::apply`'s order
insensitivity). -/
theorem write_comm (r : Reg) (a b : Write) : write (write r a) b = write (write r b) a := by
  unfold write
  rw [merge_assoc, merge_comm (some a), ← merge_assoc]

/-- Re-delivering a write is a no-op. -/
theorem write_idem (r : Reg) (a : Write) : write (write r a) a = write r a := by
  unfold write
  rw [merge_assoc, merge_idem]

/-- The join of all writes in a list. -/
def joinAll (l : List Write) : Reg :=
  l.foldr (fun w acc => merge (some w) acc) none

theorem foldl_write_eq (l : List Write) : ∀ r : Reg, l.foldl write r = merge r (joinAll l) := by
  induction l with
  | nil => intro r; cases r <;> rfl
  | cons w l ih =>
    intro r
    simp only [List.foldl_cons, joinAll, List.foldr_cons] at ih ⊢
    rw [ih, write, merge_assoc]

theorem le_joinAll {w : Write} {l : List Write} (h : w ∈ l) :
    merge (some w) (joinAll l) = joinAll l := by
  induction l with
  | nil => cases h
  | cons v l ih =>
    simp only [joinAll, List.foldr_cons] at ih ⊢
    rcases List.mem_cons.mp h with h | h
    · subst h
      rw [← merge_assoc, merge_idem]
    · rw [← merge_assoc, merge_comm (some w), merge_assoc, ih h]

theorem joinAll_le {l₁ l₂ : List Write} (h : ∀ w, w ∈ l₁ → w ∈ l₂) :
    merge (joinAll l₁) (joinAll l₂) = joinAll l₂ := by
  induction l₁ with
  | nil => rfl
  | cons v l ih =>
    simp only [joinAll, List.foldr_cons] at ih ⊢
    rw [merge_assoc, ih (fun w hw => h w (List.mem_cons_of_mem _ hw))]
    exact le_joinAll (h v List.mem_cons_self)

/-- **Convergence of LWW registers** (`Lww.converges`, cited by
`crates/jade-collab-core/src/lww.rs`): replicas that received the same writes,
in any order and with any duplication, hold the same register. -/
theorem converges {l₁ l₂ : List Write} (r : Reg) (h : ∀ w, w ∈ l₁ ↔ w ∈ l₂) :
    l₁.foldl write r = l₂.foldl write r := by
  rw [foldl_write_eq, foldl_write_eq]
  have h₁ := joinAll_le (fun w hw => (h w).mp hw)
  have h₂ := joinAll_le (fun w hw => (h w).mpr hw)
  rw [merge_comm] at h₂
  rw [← h₁, h₂]

end JadeCollab.Lww
