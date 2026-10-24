import JadeCollab.TextCursor
open JadeCollab.TextCursor

def parseAtom (word : String) : Option Atom := do
  let [id, kind, width, visible] := word.splitOn ":" | none
  let id ← id.toNat?
  let width ← width.toNat?
  let visible ← match visible with | "0" => some false | "1" => some true | _ => none
  let kind ← match kind with
    | "c" => some (Kind.character width)
    | "m" => if width = 0 then some Kind.mark else none
    | "a" => if width = 0 then some Kind.anchor else none
    | _ => none
  return ⟨id, kind, visible⟩

def parseRef (word : String) : Option (Option Nat) :=
  if word = "-" then some none else word.toNat?.map some

def main : IO UInt32 := do
  let input ← IO.getStdin
  let output ← IO.getStdout
  let mut atoms : List Atom := []
  let mut rendered := false
  repeat
    let line ← input.getLine
    if line.isEmpty then break
    let words := (line.trimAscii.toString.splitOn " ").filter (· ≠ "")
    match words with
    | "s" :: visible :: entries =>
      match visible, entries.mapM parseAtom with
      | "0", some parsed => atoms := parsed; rendered := false
      | "1", some parsed => atoms := parsed; rendered := true
      | _, _ => IO.eprintln "cursor-oracle: invalid story"; return 2
    | ["q", left, right] =>
      match parseRef left, parseRef right with
      | some left, some right =>
        match resolve rendered atoms ⟨left, right⟩ with
        | none => output.putStrLn "none"
        | some p => output.putStrLn s!"{p.paragraph} {p.offset}"
      | _, _ => IO.eprintln "cursor-oracle: invalid endpoint"; return 2
    | _ => IO.eprintln "cursor-oracle: unknown command"; return 2
  return 0
