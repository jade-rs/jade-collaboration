import JadeCollab.Upload

/-! Bounded newline records reuse the proved upload buffer, allowing one
extra byte until a possible CRLF delimiter is recognized. Flags abstract actual
delimiter inspection; Rust tests check bytes, UTF-8, EOF and cancellation. -/
namespace JadeCollab.InputFrame

def collect (limit : Nat) (buffer : Option Nat) (chunks : List Nat) : Option Nat :=
  Upload.collect (limit + 1) buffer chunks

def finish (limit : Nat) (buffer : Option Nat) (newline trailingCR : Bool) : Option Nat :=
  match buffer with
  | none => none
  | some used =>
    let payload := if newline && trailingCR then used - 1 else used
    if payload ≤ limit then some payload else none

def boundary (limit : Nat) (buffer : Option Nat) (newline trailingCR : Bool) :
    Option Nat × Option Nat :=
  (some 0, finish limit buffer newline trailingCR)

theorem buffered_bytes_are_bounded (limit : Nat) (chunks : List Nat) (bytes : Nat)
    (h : collect limit (some 0) chunks = some bytes) : bytes ≤ limit + 1 := by
  exact Upload.accepted_body_is_bounded (limit + 1) bytes chunks h

theorem refused_record_cannot_recover_before_its_boundary (limit : Nat) (chunks : List Nat) :
    collect limit none chunks = none := Upload.refused_is_absorbing (limit + 1) chunks

theorem accepted_payload_is_bounded (limit bytes : Nat) (buffer : Option Nat)
    (newline trailingCR : Bool) (h : finish limit buffer newline trailingCR = some bytes) :
    bytes ≤ limit := by
  cases buffer with
  | none => simp [finish] at h
  | some used =>
    unfold finish at h
    split at h <;> simp_all <;> omega

theorem exact_limit_record_is_accepted (limit : Nat) (chunks : List Nat)
    (h : chunks.sum = limit) : finish limit (collect limit (some 0) chunks) false false = some limit := by
  have buffered : collect limit (some 0) chunks = some limit := by
    simpa [collect, Upload.begin, h] using Upload.fragmentation_preserves_admission (limit + 1) chunks
  simp [buffered, finish]

theorem exact_limit_crlf_record_is_accepted (limit : Nat) (chunks : List Nat)
    (h : chunks.sum = limit + 1) : finish limit (collect limit (some 0) chunks) true true = some limit := by
  have buffered : collect limit (some 0) chunks = some (limit + 1) := by
    simpa [collect, Upload.begin, h] using Upload.fragmentation_preserves_admission (limit + 1) chunks
  simp [buffered, finish]

theorem extra_non_delimiter_byte_is_refused (limit : Nat) (chunks : List Nat)
    (h : chunks.sum = limit + 1) : finish limit (collect limit (some 0) chunks) true false = none := by
  have buffered : collect limit (some 0) chunks = some (limit + 1) := by
    simpa [collect, Upload.begin, h] using Upload.fragmentation_preserves_admission (limit + 1) chunks
  simp [buffered, finish]

theorem boundary_resets_even_a_refused_record (limit : Nat) (newline trailingCR : Bool) :
    (boundary limit none newline trailingCR).1 = some 0 ∧
    (boundary limit none newline trailingCR).2 = none := by simp [boundary, finish]

theorem following_record_is_independent_of_previous_failure (limit : Nat) (buffer : Option Nat)
    (newline trailingCR : Bool) (chunks : List Nat) :
    collect limit (boundary limit buffer newline trailingCR).1 chunks =
      collect limit (some 0) chunks := rfl

end JadeCollab.InputFrame
