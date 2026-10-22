import Std

/-! Abstract weighted document inbox. Natural weights stand for wire bytes;
reservations own count slots, processing retains bytes but releases a slot.
Rust PBT checks the concrete semaphore/channel against this resource model;
socket tests check authentication while admission is suspended. -/
namespace JadeCollab.Inbox

structure State where
  reserved : List Nat := []
  queued : List Nat := []
  active : List Nat := []
  accepting : Bool := true
  deriving DecidableEq

def bytes (s : State) : Nat := s.reserved.sum + s.queued.sum + s.active.sum
def slots (s : State) : Nat := s.reserved.length + s.queued.length

def reserve (count budget weight : Nat) (s : State) : Option State :=
  if s.accepting = true ∧ slots s < count ∧ bytes s + weight ≤ budget then
    some { s with reserved := weight :: s.reserved }
  else none

def cancel (s : State) : State := { s with reserved := s.reserved.tail }

def publish (s : State) : State :=
  match s.reserved with
  | [] => s
  | weight :: rest =>
    if s.accepting then { s with reserved := rest, queued := s.queued ++ [weight] }
    else { s with reserved := rest }

def take (s : State) : State :=
  match s.queued with
  | [] => s
  | weight :: rest => { s with queued := rest, active := weight :: s.active }

def release (s : State) : State := { s with active := s.active.tail }
def stop (s : State) : State := { s with accepting := false }

theorem admitted_resources_fit (s next : State) (count budget weight : Nat)
    (h : reserve count budget weight s = some next) :
    slots next ≤ count ∧ bytes next ≤ budget := by
  unfold reserve at h
  split at h
  · rename_i fits
    have eq := Option.some.inj h
    subst next
    simp only [slots, bytes, List.length_cons, List.sum_cons]
    simp only [slots, bytes] at fits
    omega
  · simp at h

theorem closed_cannot_reserve (s : State) (count budget weight : Nat) :
    reserve count budget weight (stop s) = none := by simp [reserve, stop]

theorem cancel_returns_reservation_bytes (s : State) (weight : Nat) (rest : List Nat)
    (h : s.reserved = weight :: rest) : bytes (cancel s) + weight = bytes s := by
  simp [bytes, cancel, h]; omega

theorem cancel_returns_count_slot (s : State) (weight : Nat) (rest : List Nat)
    (h : s.reserved = weight :: rest) : slots (cancel s) + 1 = slots s := by
  simp [slots, cancel, h]; omega

theorem publication_preserves_bytes_and_slots (s : State) (h : s.accepting = true) :
    bytes (publish s) = bytes s ∧ slots (publish s) = slots s := by
  cases hr : s.reserved with
  | nil => simp [publish, hr]
  | cons weight rest => simp [publish, hr, h, bytes, slots, List.sum_append]; omega

theorem publication_is_fifo (s : State) (weight : Nat) (rest : List Nat)
    (h : s.accepting = true) (hr : s.reserved = weight :: rest) :
    (publish s).queued = s.queued ++ [weight] := by simp [publish, hr, h]

theorem closed_publication_cannot_extend_the_queue (s : State) :
    (publish (stop s)).queued = s.queued := by
  cases hr : s.reserved <;> simp [publish, stop, hr]

theorem taking_retains_byte_lease (s : State) : bytes (take s) = bytes s := by
  cases h : s.queued with
  | nil => simp [take, h]
  | cons weight rest => simp [take, h, bytes]; omega

theorem taking_releases_count_slot (s : State) (weight : Nat) (rest : List Nat)
    (h : s.queued = weight :: rest) : slots (take s) + 1 = slots s := by
  simp [take, h, slots]; omega

theorem processing_completion_returns_bytes (s : State) (weight : Nat) (rest : List Nat)
    (h : s.active = weight :: rest) : bytes (release s) + weight = bytes s := by
  simp [bytes, release, h]; omega

theorem stop_preserves_accepted_fifo (s : State) : (stop s).queued = s.queued := rfl

theorem drain_does_not_need_uncommitted_reservations (s : State) (h : s.queued = []) :
    take (stop s) = stop s := by simp [take, stop, h]

end JadeCollab.Inbox
