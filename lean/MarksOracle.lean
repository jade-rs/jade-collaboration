import JadeCollab.Marks

/-!
# `marks-oracle`: the Lean model of marks as an executable oracle

Runs the very `Rga.walk` and `Marks.effective` that `JadeCollab/Rga.lean`
and `JadeCollab/Marks.lean` prove things about, so the Rust RGA and mark set
can be compared against them.

Input (stdin), numbers in decimal, one operation per line, in any order
(the result is a function of the sets — that is what is proved):

```
i <lamport> <replica> <parent-lamport> <parent-replica>   insert after an atom
i <lamport> <replica> -                                   insert at the story start
m <lamport> <replica> <key> <value> <start> <end>         a mark
k <key> <key> ...                                         the attributes to report
```

A boundary is `s` (story start), `b:<lamport>:<replica>` (before an atom),
`a:<lamport>:<replica>` (after an atom) or `e` (story end).

Output (stdout): one line per atom, in story order (tombstones are atoms
too; deletions do not affect formatting): `<lamport>:<replica>` followed, for
each reported attribute, by `-` (no mark covers the atom) or
`<lamport>:<replica>:<value>` (the winning mark). A malformed line is
reported on stderr with exit code 2.
-/

open JadeCollab JadeCollab.Marks

def words (line : String) : List String :=
  ((line.replace "\t" " " |>.replace "\r" "" |>.replace "\n" "").splitOn " ").filter (· ≠ "")

def parseId (l r : String) : Option Rga.Id := return ⟨← l.toNat?, ← r.toNat?⟩

def parseBoundary (s : String) : Option Boundary :=
  match s.splitOn ":" with
  | ["s"] => some .start
  | ["e"] => some .stop
  | ["b", l, r] => (parseId l r).map .before
  | ["a", l, r] => (parseId l r).map .after
  | _ => none

inductive Line where
  | insert (n : Rga.Node)
  | mark (m : Mark)
  | keys (ks : List Nat)

def parseLine : List String → Option Line
  | ["i", l, r, "-"] => return .insert ⟨← parseId l r, none, 0⟩
  | ["i", l, r, pl, pr] => return .insert ⟨← parseId l r, some (← parseId pl pr), 0⟩
  | ["m", l, r, k, v, s, e] =>
    return .mark ⟨← parseId l r, ← k.toNat?, ← v.toNat?, ← parseBoundary s, ← parseBoundary e⟩
  | "k" :: ks => return .keys (← ks.mapM String.toNat?)
  | _ => none

def showReg : Lww.Reg → String
  | none => "-"
  | some (l, r, v) => s!"{l}:{r}:{v}"

partial def readLines (h : IO.FS.Stream) (acc : Array String) : IO (Array String) := do
  let line ← h.getLine
  if line.isEmpty then return acc
  else readLines h (acc.push line)

def fail (msg : String) : IO UInt32 := do
  IO.eprintln s!"marks-oracle: {msg}"
  return 2

def main : IO UInt32 := do
  let lines ← readLines (← IO.getStdin) #[]
  let mut inserts : List Rga.Node := []
  let mut marks : List Mark := []
  let mut keys : List Nat := []
  for ws in lines.toList.map words |>.filter (· ≠ []) do
    match parseLine ws with
    | some (.insert n) => inserts := inserts ++ [n]
    | some (.mark m) => marks := marks ++ [m]
    | some (.keys ks) => keys := ks
    | none => return (← fail s!"bad line: {" ".intercalate ws}")
  let s := Rga.state inserts
  let seq := order s
  let ms := markState marks
  let out ← IO.getStdout
  for x in seq do
    let regs := keys.map fun k => showReg (effective seq ms x k)
    out.putStrLn (" ".intercalate (s!"{x.lamport}:{x.replica}" :: regs))
  return 0
