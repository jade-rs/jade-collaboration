import JadeCollab.Journal

/-! Runs the proved journal model. `a <values>` commits a batch,
`t <values>` interrupts a write, `c` checkpoints and `r` recovers.
Prints the recovered operation list after every command. -/

open JadeCollab.Journal

def step (state : List Nat) (op : Nat) : List Nat := state ++ [op]

def main : IO UInt32 := do
  let input ← IO.getStdin
  let output ← IO.getStdout
  let mut store : Store (List Nat) Nat := ⟨[], [], []⟩
  repeat
    let line ← input.getLine
    if line.isEmpty then break
    let words := ((line.trimAscii.toString).splitOn " ").filter (· ≠ "")
    match words with
    | "a" :: values | "t" :: values =>
      match values.mapM String.toNat? with
      | none => IO.eprintln "journal-oracle: invalid number"; return 2
      | some batch =>
        if words.head? == some "a" then store := appendBatch store batch
        else store := interrupt store batch
    | ["c"] => store := checkpoint step store
    | ["r"] => store := { store with unfinished := [] }
    | _ => IO.eprintln "journal-oracle: invalid command"; return 2
    output.putStrLn (" ".intercalate ((recover step store).map toString))
  return 0
