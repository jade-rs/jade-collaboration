import JadeCollab.IdAllocation

open JadeCollab.IdAllocation

def main : IO UInt32 := do
  let input ← IO.getStdin
  let output ← IO.getStdout
  let mut time := 0
  repeat
    let line ← input.getLine
    if line.isEmpty then break
    let words := (line.trimAscii.toString.splitOn " ").filter (· ≠ "")
    match words with
    | ["o", value] =>
      match value.toNat? with
      | none => IO.eprintln "id-oracle: invalid observation"; return 2
      | some value => time := max time value; output.putStrLn s!"o {time}"
    | ["r", count] =>
      match count.toNat? with
      | none => IO.eprintln "id-oracle: invalid reservation"; return 2
      | some count =>
        match reserve u64Max time count with
        | none => output.putStrLn s!"err {time}"
        | some (first, last) => time := last; output.putStrLn s!"ok {first} {last}"
    | ["i", first, count] =>
      match first.toNat?, count.toNat? with
      | some first, some count =>
        match runEnd u64Max first count with
        | none => output.putStrLn "err"
        | some last => output.putStrLn s!"id {last}"
      | _, _ => IO.eprintln "id-oracle: invalid run"; return 2
    | _ => IO.eprintln "id-oracle: invalid command"; return 2
  return 0
