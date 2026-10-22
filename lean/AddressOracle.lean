import JadeCollab.Address

open JadeCollab.Address

def main : IO UInt32 := do
  let input ← IO.getStdin
  let output ← IO.getStdout
  repeat
    let line ← input.getLine
    if line.isEmpty then break
    let words := (line.trimAscii.toString.splitOn " ").filter (· ≠ "")
    let mut bytes : List (Fin 256) := []
    for word in words do
      match word.toNat? with
      | some n =>
        if h : n < 256 then bytes := bytes ++ [⟨n, h⟩]
        else IO.eprintln "address-oracle: byte out of range"; return 2
      | none => IO.eprintln "address-oracle: invalid byte"; return 2
    output.putStrLn (String.ofList ((encode bytes).map Char.ofNat))
  return 0
