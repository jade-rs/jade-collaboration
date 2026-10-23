import ProofAudit

namespace ProofAuditFixture.Foreign
axiom assumption : False
opaque indirect : False := assumption
end ProofAuditFixture.Foreign

namespace ProofAuditFixture.Public
theorem claim : False := ProofAuditFixture.Foreign.assumption
end ProofAuditFixture.Public

/-- error: ProofAuditFixture.Public.claim depends on forbidden axioms: [ProofAuditFixture.Foreign.assumption] -/
#guard_msgs in
#audit_axioms ProofAuditFixture.Public

namespace ProofAuditFixture.Private
private axiom assumption : False
end ProofAuditFixture.Private

/-- error: ProofAuditFixture.Private.assumption depends on forbidden axioms: [ProofAuditFixture.Private.assumption] -/
#guard_msgs in
#audit_axioms ProofAuditFixture.Private

namespace ProofAuditFixture.Opaque
theorem claim : False := ProofAuditFixture.Foreign.indirect
end ProofAuditFixture.Opaque

/-- error: ProofAuditFixture.Opaque.claim depends on forbidden axioms: [ProofAuditFixture.Foreign.assumption] -/
#guard_msgs in
#audit_axioms ProofAuditFixture.Opaque

/-- error: axiom audit found no declarations under ProofAuditFixture.Missing -/
#guard_msgs in
#audit_axioms ProofAuditFixture.Missing

namespace ProofAuditFixture.Allowed
theorem identity (p : Prop) (h : p) : p := h
theorem classicalChoice {α : Type} (witness : Nonempty α) : Nonempty α := ⟨Classical.choice witness⟩
theorem extensionality (p q : Prop) (same : p ↔ q) : p = q := propext same
end ProofAuditFixture.Allowed

/-- info: Axiom audit checked 3 declarations under ProofAuditFixture.Allowed. -/
#guard_msgs in
#audit_axioms ProofAuditFixture.Allowed

-- A declaration outside the expected namespace still belongs to its module.
run_cmd do
  let names ← ProofAudit.declarationsUnder `AuditTests
  unless names.contains `ProofAuditFixture.Public.claim do
    throwError "module ownership must cover declarations outside its namespace"
