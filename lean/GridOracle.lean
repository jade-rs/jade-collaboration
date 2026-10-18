import JadeCollab.Grid

/-!
# `grid-oracle`: the Lean model of `Grid` as an executable oracle

Runs the very `apply` and `check` that `JadeCollab/Grid.lean` proves things
about, so the Rust implementation can be compared against it line by line.

Input (stdin), identifiers in decimal:

```
rows <id> <id> ...          initial rows, all alive, in order
cols <id> <id> ...          initial columns
ir <id> <after|->           InsertRow   (`-`: after = None)
dr <id>                     DeleteRow
ic <id> <after|->           InsertColumn
dc <id>                     DeleteColumn
m <top> <left> <bottom> <right>   Merge
u <top> <left>              Unmerge
```

Output (stdout): `ok` or `err` per operation, then
`rows <id>:<1|0> ...`, `cols <id>:<1|0> ...` (alive flags, every line ever
inserted, in order), `merges <top>,<left>,<bottom>,<right> ...` (list order),
and `valid` / `invalid` (`check`, equivalent to `Valid` by `check_iff_valid`).
A malformed line is reported on stderr with exit code 2.
-/

open JadeCollab JadeCollab.Grid

def words (line : String) : List String :=
  ((line.replace "\t" " " |>.replace "\r" "" |>.replace "\n" "").splitOn " ").filter (· ≠ "")

def parseAfter (s : String) : Option (Option Nat) :=
  if s = "-" then some none else s.toNat?.map some

def parseIds (ws : List String) : Option (List Nat) := ws.mapM String.toNat?

def parseOp : List String → Option Op
  | ["ir", id, a] => return .insertRow (← id.toNat?) (← parseAfter a)
  | ["dr", id] => return .deleteRow (← id.toNat?)
  | ["ic", id, a] => return .insertCol (← id.toNat?) (← parseAfter a)
  | ["dc", id] => return .deleteCol (← id.toNat?)
  | ["m", t, l, b, r] => return .merge ⟨← t.toNat?, ← l.toNat?, ← b.toNat?, ← r.toNat?⟩
  | ["u", t, l] => return .unmerge (← t.toNat?) (← l.toNat?)
  | _ => none

def showLines (tag : String) (ls : List Line) : String :=
  " ".intercalate (tag :: ls.map fun l => s!"{l.id}:{if l.alive then 1 else 0}")

def showMerges (ms : List Merge) : String :=
  " ".intercalate ("merges" :: ms.map fun m => s!"{m.top},{m.left},{m.bottom},{m.right}")

partial def readLines (h : IO.FS.Stream) (acc : Array String) : IO (Array String) := do
  let line ← h.getLine
  if line.isEmpty then return acc
  else readLines h (acc.push line)

def fail (msg : String) : IO UInt32 := do
  IO.eprintln s!"grid-oracle: {msg}"
  return 2

def main : IO UInt32 := do
  let lines ← readLines (← IO.getStdin) #[]
  let lines := lines.toList.map words |>.filter (· ≠ [])
  match lines with
  | ("rows" :: rs) :: ("cols" :: cs) :: ops =>
    let some rows := parseIds rs | fail "bad rows line"
    let some cols := parseIds cs | fail "bad cols line"
    let out ← IO.getStdout
    let mut g := Grid.new rows cols
    for ws in ops do
      let some op := parseOp ws | return (← fail s!"bad operation: {" ".intercalate ws}")
      match apply g op with
      | some g' => out.putStrLn "ok"; g := g'
      | none => out.putStrLn "err"
    out.putStrLn (showLines "rows" g.rows)
    out.putStrLn (showLines "cols" g.cols)
    out.putStrLn (showMerges g.merges)
    out.putStrLn (if check g then "valid" else "invalid")
    return 0
  | _ => fail "expected a `rows` line and a `cols` line first"
