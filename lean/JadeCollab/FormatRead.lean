import JadeCollab.Paging

/-! Reading collaborative overrides after the mark winner has been computed.
Unset attributes carry no claim about the file's base formatting. Rust PBT
checks that compressed UTF-8 spans decode to the mark specification. -/
namespace JadeCollab.FormatRead

structure Span (α : Type) where
  first : Nat
  last : Nat
  value : α
deriving DecidableEq

def report (spans : List (Span (Option α))) : List (Span α) :=
  spans.filterMap fun span => span.value.map fun value => ⟨span.first, span.last, value⟩

theorem unset_is_omitted (first last : Nat) :
    report [Span.mk first last (none : Option α)] = [] := by
  simp [report]

theorem explicit_false_is_kept (first last : Nat) :
    report [Span.mk first last (some false)] = [Span.mk first last false] := by
  simp [report]

theorem report_preserves_boundaries (out : Span α) (spans : List (Span (Option α)))
    (h : out ∈ report spans) :
    ∃ source ∈ spans, source.first = out.first ∧ source.last = out.last ∧
      source.value = some out.value := by
  obtain ⟨source, hs, equal⟩ := List.mem_filterMap.mp h
  cases source with
  | mk first last value =>
    cases value with
    | none => simp at equal
    | some v =>
      simp only [Option.map_some, Option.some.injEq] at equal
      cases equal
      exact ⟨_, hs, rfl, rfl, rfl⟩

/-- Attaching formatting to paragraphs before paging gives the same page as
paging paragraphs first. No annotation leaks in from an unselected paragraph. -/
theorem page_projection (paragraphs : List (List (Span (Option α)))) (start limit : Nat) :
    Paging.slice (paragraphs.map report) start limit =
      (Paging.slice paragraphs start limit).map report := by
  simp [Paging.slice, List.map_take, List.map_drop]

/-- Interpreting an omitted override requires the base; omission alone does
not establish that a boolean property is false. -/
def resolve (base override : Option α) : Option α := override.or base

theorem omitted_inherits (base : Option α) : resolve base none = base := rfl

theorem explicit_false_overrides (base : Option Bool) :
    resolve base (some false) = some false := rfl

theorem unknown_base_stays_unknown : resolve (none : Option Bool) none = none := rfl

/-- A duplicated slide can share shape identifiers with its source. Read
annotations must therefore be keyed by both slide and shape identity. -/
def appendShape [DecidableEq κ] (formats : (κ × κ) → List α)
    (owner : κ × κ) (run : α) : (κ × κ) → List α :=
  fun key => if key = owner then formats key ++ [run] else formats key

theorem appended_run_is_owned [DecidableEq κ] (formats : (κ × κ) → List α)
    (owner : κ × κ) (run : α) :
    appendShape formats owner run owner = formats owner ++ [run] := by
  simp [appendShape]

theorem another_slide_is_unchanged [DecidableEq κ] (formats : (κ × κ) → List α)
    (slide other shape : κ) (run : α) (h : slide ≠ other) :
    appendShape formats (slide, shape) run (other, shape) = formats (other, shape) := by
  have different : (other, shape) ≠ (slide, shape) := by
    intro equal
    exact h (congrArg Prod.fst equal).symm
  simp [appendShape, different]

theorem appended_runs_keep_order [DecidableEq κ] (formats : (κ × κ) → List α)
    (owner : κ × κ) (first second : α) :
    appendShape (appendShape formats owner first) owner second owner =
      formats owner ++ [first, second] := by
  simp [appendShape, List.append_assoc]

/-- Native run offsets are global in a text body. Read paragraphs clip each
run to their own half-open range and translate it to paragraph-local bytes.
Empty or reversed intersections cover no byte. -/
def clip (span : Span α) (origin length : Nat) : Span α :=
  ⟨max span.first origin - origin, min span.last (origin + length) - origin, span.value⟩

def covers (span : Span α) (offset : Nat) : Prop :=
  span.first ≤ offset ∧ offset < span.last

theorem clipped_coverage (span : Span α) (origin length offset : Nat) :
    covers (clip span origin length) offset ↔
      offset < length ∧ covers span (origin + offset) := by
  simp only [covers, clip]
  omega

theorem clipping_preserves_style (span : Span α) (origin length : Nat) :
    (clip span origin length).value = span.value := rfl

end JadeCollab.FormatRead
