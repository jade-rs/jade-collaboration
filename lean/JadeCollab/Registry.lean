/-! Document names are reserved before native initialization, which runs
outside the registry transition. Only the reservation owner can publish or
cancel it. Rust uses opaque Arc identities, RAII cleanup, and short mutex
transitions; tests cover actual blocking I/O and independent socket edits. -/
namespace JadeCollab.Registry

inductive Slot (α : Type) where
  | opening (owner : Nat)
  | ready (value : α)

abbrev State (α : Type) := Nat → Option (Slot α)

def write (state : State α) (name : Nat) (slot : Option (Slot α)) : State α :=
  fun key => if key = name then slot else state key

def visible (state : State α) (name : Nat) : Option α :=
  match state name with
  | some (.ready value) => some value
  | _ => none

def reserve (state : State α) (name owner : Nat) : State α × Bool :=
  match state name with
  | none => (write state name (some (.opening owner)), true)
  | some _ => (state, false)

def publish (state : State α) (name owner : Nat) (value : α) : State α :=
  match state name with
  | some (.opening held) =>
    if held = owner then write state name (some (.ready value)) else state
  | _ => state

def cancel (state : State α) (name owner : Nat) : State α :=
  match state name with
  | some (.opening held) => if held = owner then write state name none else state
  | _ => state

theorem occupied_name_refuses_reservation (state : State α) (name owner : Nat)
    (slot : Slot α) (h : state name = some slot) :
    reserve state name owner = (state, false) := by simp [reserve, h]

theorem reserved_name_is_hidden (state : State α) (name owner : Nat)
    (h : state name = none) :
    visible (reserve state name owner).1 name = none := by
  simp [reserve, h, visible, write]

theorem another_name_survives_reservation (state : State α) (name other owner : Nat)
    (h : other ≠ name) : (reserve state name owner).1 other = state other := by
  cases hs : state name <;> simp [reserve, hs, write, h]

theorem owner_publishes (state : State α) (name owner : Nat) (value : α)
    (h : state name = some (.opening owner)) :
    visible (publish state name owner value) name = some value := by
  simp [publish, h, visible, write]

theorem foreign_owner_cannot_publish (state : State α) (name held owner : Nat)
    (value : α) (h : state name = some (.opening held)) (different : held ≠ owner) :
    publish state name owner value = state := by simp [publish, h, different]

theorem published_room_cannot_be_replaced (state : State α) (name owner : Nat)
    (old value : α) (h : state name = some (.ready old)) :
    publish state name owner value = state := by simp [publish, h]

theorem owner_cancel_permits_retry (state : State α) (name owner fresh : Nat)
    (h : state name = some (.opening owner)) :
    (reserve (cancel state name owner) name fresh).2 = true := by
  simp [cancel, h, reserve, write]

theorem foreign_cancel_is_unchanged (state : State α) (name held owner : Nat)
    (h : state name = some (.opening held)) (different : held ≠ owner) :
    cancel state name owner = state := by simp [cancel, h, different]

theorem cancel_preserves_ready_room (state : State α) (name owner : Nat)
    (value : α) (h : state name = some (.ready value)) :
    cancel state name owner = state := by simp [cancel, h]

theorem another_name_survives_publication (state : State α) (name other owner : Nat)
    (value : α) (h : other ≠ name) :
    publish state name owner value other = state other := by
  cases hs : state name with
  | none => simp [publish, hs]
  | some slot =>
    cases slot with
    | opening held =>
      by_cases same : held = owner
      · simp [publish, hs, same, write, h]
      · simp [publish, hs, same]
    | ready old => simp [publish, hs]

theorem another_name_survives_cancellation (state : State α) (name other owner : Nat)
    (h : other ≠ name) : cancel state name owner other = state other := by
  cases hs : state name with
  | none => simp [cancel, hs]
  | some slot =>
    cases slot with
    | opening held =>
      by_cases same : held = owner
      · simp [cancel, hs, same, write, h]
      · simp [cancel, hs, same]
    | ready value => simp [cancel, hs]

end JadeCollab.Registry
