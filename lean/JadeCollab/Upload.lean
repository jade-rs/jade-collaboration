import Std

/-! Specification of bounded upload buffering. Only an authorized, complete
body can be published for opening. Rust tests exercise axum's real extractor,
stream errors, HTTP chunking and absence of partially initialized storage. -/
namespace JadeCollab.Upload

def begin (authorized : Bool) : Option Nat :=
  if authorized then some 0 else none

def push (limit : Nat) (buffer : Option Nat) (bytes : Nat) : Option Nat :=
  buffer.bind fun used => if used + bytes ≤ limit then some (used + bytes) else none

def collect (limit : Nat) (buffer : Option Nat) (frames : List Nat) : Option Nat :=
  frames.foldl (push limit) buffer

def publish (complete : Bool) (buffer : Option Nat) : Option Nat :=
  if complete then buffer else none

theorem forbidden_cannot_begin : begin false = none := rfl

theorem refused_is_absorbing (limit : Nat) (frames : List Nat) :
    collect limit none frames = none := by
  induction frames with
  | nil => rfl
  | cons frame frames ih => simpa [collect, push] using ih

theorem complete_buffer_exact (limit used : Nat) (frames : List Nat)
    (h : used + frames.sum ≤ limit) :
    collect limit (some used) frames = some (used + frames.sum) := by
  induction frames generalizing used with
  | nil => simp [collect]
  | cons frame frames ih =>
    have hPrefix : used + frame ≤ limit := by simp only [List.sum_cons] at h; omega
    have rest : used + frame + frames.sum ≤ limit := by simp only [List.sum_cons] at h; omega
    simpa [collect, push, hPrefix, Nat.add_assoc] using ih (used + frame) rest

theorem over_limit_is_refused (limit used : Nat) (frames : List Nat)
    (valid : used ≤ limit) (h : limit < used + frames.sum) :
    collect limit (some used) frames = none := by
  induction frames generalizing used with
  | nil => simp only [List.sum_nil, Nat.add_zero] at h; omega
  | cons frame frames ih =>
    by_cases hPrefix : used + frame ≤ limit
    · have rest : limit < used + frame + frames.sum := by simp only [List.sum_cons] at h; omega
      simpa [collect, push, hPrefix] using ih (used + frame) hPrefix rest
    · simpa [collect, push, hPrefix] using refused_is_absorbing limit frames

theorem fragmentation_preserves_admission (limit : Nat) (frames : List Nat) :
    collect limit (begin true) frames =
      if frames.sum ≤ limit then some frames.sum else none := by
  by_cases h : frames.sum ≤ limit
  · simpa [begin, h] using complete_buffer_exact limit 0 frames (by simpa using h)
  · simpa [begin, h] using over_limit_is_refused limit 0 frames (by omega) (by omega)

theorem exact_limit_is_accepted (limit : Nat) (frames : List Nat) (h : frames.sum = limit) :
    collect limit (begin true) frames = some limit := by
  simp [fragmentation_preserves_admission, h]

theorem accepted_body_is_bounded (limit bytes : Nat) (frames : List Nat)
    (h : collect limit (begin true) frames = some bytes) : bytes ≤ limit := by
  rw [fragmentation_preserves_admission] at h
  split at h
  · cases h; assumption
  · contradiction

theorem incomplete_never_publishes (buffer : Option Nat) : publish false buffer = none := rfl

theorem forbidden_never_publishes (limit : Nat) (frames : List Nat) (complete : Bool) :
    publish complete (collect limit (begin false) frames) = none := by
  simp [begin, refused_is_absorbing, publish]

end JadeCollab.Upload
