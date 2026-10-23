import Lean

/-! Inspect the compiled environment, including private definitions and
transitive opaque dependencies. A text search alone cannot enforce this trust
boundary. Audit fixtures live outside the production namespace. -/
namespace ProofAudit
open Lean Elab Command

def permitted : Array Name := #[``propext, ``Classical.choice, ``Quot.sound]

def declarationsUnder (root : Name) : CommandElabM (Array Name) := do
  let env ← getEnv
  let names := env.constants.fold (init := #[]) fun names name _ =>
    let owner := (env.getModuleIdxFor? name).map (env.header.moduleNames[·]!) |>.getD env.mainModule
    if root.isPrefixOf (privateToUserName name) || root.isPrefixOf owner then names.push name else names
  return names.qsort fun a b => Name.quickLt (privateToUserName a) (privateToUserName b)

def forbiddenDependencies (decl : Name) : CommandElabM (Array Name) := do
  let dependencies ← collectAxioms decl
  return (dependencies.filter fun name => !permitted.contains name).qsort Name.quickLt

elab "#audit_axioms " root:ident : command => do
  let names ← declarationsUnder root.getId
  if names.isEmpty then
    throwError "axiom audit found no declarations under {root.getId}"
  for name in names do
    let forbidden ← forbiddenDependencies name
    unless forbidden.isEmpty do
      throwError "{privateToUserName name} depends on forbidden axioms: {forbidden.map privateToUserName}"
  logInfo m!"Axiom audit checked {names.size} declarations under {root.getId}."

end ProofAudit
