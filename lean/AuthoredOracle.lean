import JadeCollab.AuthoredAdmission

open JadeCollab.AuthoredAdmission

def verdictText : Verdict → String
  | .accepted => "ok"
  | .refused => "err"
  | .duplicate => "dup"

def stateText (s : State) (verdict : String) : String :=
  s!"{verdict} {s.seq} {s.processed} {if s.authorMatches then 1 else 0} {s.edits}"

def main : IO UInt32 := do
  let input ← IO.getStdin
  let output ← IO.getStdout
  let mut limit := 0
  let mut state : State := ⟨0,0,false,0⟩
  repeat
    let line ← input.getLine
    if line.isEmpty then break
    let words := (line.trimAscii.toString.splitOn " ").filter (· ≠ "")
    match words with
    | ["init",max,seq,named] =>
      match max.toNat?,seq.toNat?,named.toNat? with
      | some max,some seq,some named =>
        if seq > max || named > 1 then return 2
        limit := max
        state := ⟨seq,0,named == 1,0⟩
        output.putStrLn (stateText state "init")
      | _,_,_ => return 2
    | ["s",ticket,valid,ready] =>
      match ticket.toNat?,valid.toNat?,ready.toNat? with
      | some ticket,some valid,some ready =>
        if valid > 1 || ready > 1 then return 2
        let (next,verdict) := submit limit ticket (valid == 1) (ready == 1) state
        state := next
        output.putStrLn (stateText state (verdictText verdict))
      | _,_,_ => return 2
    | ["restore"] =>
      state := restore state
      output.putStrLn (stateText state "restore")
    | _ => return 2
  return 0
