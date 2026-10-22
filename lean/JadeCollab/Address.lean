/-! Canonical percent encoding of an opaque UTF-8 byte sequence. This
specifies the byte codec, not HTTP parsing or Unicode validation. Rust PBT
checks the real URI/UTF-8 implementation and durable document routing. -/
namespace JadeCollab.Address

def unreserved (b : Nat) : Prop :=
  (65 ≤ b ∧ b ≤ 90) ∨ (97 ≤ b ∧ b ≤ 122) ∨ (48 ≤ b ∧ b ≤ 57) ∨
  b = 45 ∨ b = 46 ∨ b = 95 ∨ b = 126

instance (b : Nat) : Decidable (unreserved b) := by unfold unreserved; infer_instance

def hex (n : Nat) : Nat := if n < 10 then 48 + n else 55 + n

def unhex (b : Nat) : Option Nat :=
  if 48 ≤ b ∧ b ≤ 57 then some (b - 48)
  else if 65 ≤ b ∧ b ≤ 70 then some (b - 55) else none

theorem unhex_hex (n : Nat) (h : n < 16) : unhex (hex n) = some n := by
  unfold hex unhex
  split <;> split <;> simp_all <;> omega

def encodeByte (b : Fin 256) : List Nat :=
  if unreserved b.val then [b.val]
  else [37, hex (b.val / 16), hex (b.val % 16)]

def encode (bytes : List (Fin 256)) : List Nat := bytes.flatMap encodeByte

def decode : List Nat → Option (List Nat)
  | [] => some []
  | first :: rest =>
    if first = 37 then
      match rest with
      | a :: b :: tail => do
        let hi ← unhex a
        let lo ← unhex b
        let decoded ← decode tail
        pure ((16 * hi + lo) :: decoded)
      | _ => none
    else (decode rest).map (first :: ·)

theorem decode_encoded_byte (b : Fin 256) (tail : List Nat) :
    decode (encodeByte b ++ tail) = (decode tail).map (b.val :: ·) := by
  unfold encodeByte
  split
  · rename_i h
    have hp : b.val ≠ 37 := by unfold unreserved at h; omega
    simp only [List.cons_append, List.nil_append]
    conv => lhs; unfold decode
    simp [hp]
  · have hi : b.val / 16 < 16 := by omega
    have lo : b.val % 16 < 16 := Nat.mod_lt _ (by decide)
    have combine : 16 * (b.val / 16) + b.val % 16 = b.val := by omega
    simp [decode, unhex_hex _ hi, unhex_hex _ lo, combine]
    cases decode tail <;> rfl

theorem decode_encode (bytes : List (Fin 256)) :
    decode (encode bytes) = some (bytes.map Fin.val) := by
  induction bytes with
  | nil => rfl
  | cons b rest ih =>
    simp only [encode, List.flatMap_cons]
    rw [decode_encoded_byte]
    change (decode (encode rest)).map (b.val :: ·) = _
    rw [ih]
    rfl

theorem encoded_names_are_injective (a b : List (Fin 256))
    (h : encode a = encode b) : a = b := by
  have decoded := congrArg decode h
  rw [decode_encode, decode_encode] at decoded
  have mapped : a.map Fin.val = b.map Fin.val := Option.some.inj decoded
  clear h decoded
  induction a generalizing b with
  | nil => cases b <;> simp_all
  | cons x xs ih =>
    cases b with
    | nil => simp_all
    | cons y ys =>
      simp only [List.map_cons, List.cons.injEq] at mapped
      have same : x = y := Fin.ext mapped.1
      subst y
      have rest := ih ys mapped.2
      exact congrArg (x :: ·) rest

theorem hex_has_no_delimiters (n : Nat) (h : n < 16) :
    hex n ≠ 47 ∧ hex n ≠ 63 ∧ hex n ≠ 35 := by
  unfold hex; split <;> omega

theorem encoded_byte_has_no_delimiters (b : Fin 256) (n : Nat)
    (h : n ∈ encodeByte b) : n ≠ 47 ∧ n ≠ 63 ∧ n ≠ 35 := by
  unfold encodeByte at h
  split at h
  · rename_i safe
    simp only [List.mem_singleton] at h
    subst n
    unfold unreserved at safe
    omega
  · simp only [List.mem_cons] at h
    have hi := hex_has_no_delimiters (b.val / 16) (by omega)
    have lo := hex_has_no_delimiters (b.val % 16) (Nat.mod_lt _ (by decide))
    rcases h with h | h | h | h
    · omega
    · subst n; exact hi
    · subst n; exact lo
    · cases h

theorem encoded_path_has_no_delimiters (bytes : List (Fin 256)) (n : Nat)
    (h : n ∈ encode bytes) : n ≠ 47 ∧ n ≠ 63 ∧ n ≠ 35 := by
  obtain ⟨b, _, hb⟩ := List.mem_flatMap.mp h
  exact encoded_byte_has_no_delimiters b n hb

end JadeCollab.Address
