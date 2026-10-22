/-! Secure endpoint policy. Verification booleans abstract the TLS library's
certificate-chain and hostname checks; this does not prove cryptography, URI
parsing or native certificate loading. Rust tests exercise those real paths. -/
namespace JadeCollab.Transport

inductive Scheme where
  | http | https | ws | wss
deriving DecidableEq

def secure : Scheme → Bool
  | .https | .wss => true
  | _ => false

def discovery : Scheme → Option Scheme
  | .http => some .ws
  | .https => some .wss
  | _ => none

theorem discovery_preserves_encryption (source target : Scheme)
    (h : discovery source = some target) : secure target = secure source := by
  cases source <;> cases target <;> simp_all [discovery, secure]

inductive Stage where
  | connecting | verifying | ready | failed
deriving DecidableEq

structure Connection where
  scheme : Scheme
  stage : Stage
  verified : Bool

inductive Event where
  | connected
  | verification (chain hostname : Bool)
  | failure

def advance (state : Connection) (event : Event) : Connection :=
  match state.stage, event with
  | .failed, _ => state
  | _, .failure => {state with stage := .failed}
  | .connecting, .connected =>
      {state with stage := if secure state.scheme then .verifying else .ready}
  | .verifying, .verification chain hostname =>
      if chain && hostname then {state with stage := .ready, verified := true}
      else {state with stage := .failed}
  | _, _ => state

def canSend (state : Connection) : Bool := decide (state.stage = .ready)

def Invariant (state : Connection) : Prop :=
  secure state.scheme = true → state.stage = .ready → state.verified = true

theorem advance_preserves_verification (state : Connection) (event : Event)
    (h : Invariant state) : Invariant (advance state event) := by
  cases event with
  | connected =>
    cases state with
    | mk scheme stage verified =>
      cases scheme <;> cases stage <;> simp_all [Invariant, advance, secure]
  | failure =>
    cases state with
    | mk scheme stage verified =>
      cases scheme <;> cases stage <;> simp_all [Invariant, advance, secure]
  | verification chain hostname =>
    cases chain <;> cases hostname <;>
      (cases state with
       | mk scheme stage verified =>
         cases scheme <;> cases stage <;> simp_all [Invariant, advance, secure])

theorem application_bytes_require_secure_verification (state : Connection)
    (valid : Invariant state) (encrypted : secure state.scheme = true)
    (sending : canSend state = true) : state.verified = true := by
  exact valid encrypted (by simpa [canSend] using sending)

theorem advance_never_downgrades (state : Connection) (event : Event) :
    (advance state event).scheme = state.scheme := by
  cases state with
  | mk scheme stage verified =>
    cases stage <;> cases event <;> simp [advance] <;> split <;> rfl

def initial (scheme : Scheme) : Connection :=
  {scheme, stage := .connecting, verified := false}

theorem history_preserves_verification (events : List Event) :
    ∀ state, Invariant state → Invariant (events.foldl advance state) := by
  induction events with
  | nil => intro state h; exact h
  | cons event rest ih =>
    intro state h
    exact ih (advance state event) (advance_preserves_verification state event h)

theorem history_never_downgrades (events : List Event) (state : Connection) :
    (events.foldl advance state).scheme = state.scheme := by
  induction events generalizing state with
  | nil => rfl
  | cons event rest ih =>
    simp only [List.foldl_cons]
    rw [ih, advance_never_downgrades]

theorem secure_history_requires_verification (events : List Event) (scheme : Scheme)
    (encrypted : secure scheme = true)
    (sending : canSend (events.foldl advance (initial scheme)) = true) :
    (events.foldl advance (initial scheme)).verified = true := by
  have valid := history_preserves_verification events (initial scheme) (by simp [Invariant, initial])
  apply application_bytes_require_secure_verification _ valid _ sending
  rw [history_never_downgrades]
  exact encrypted

theorem failed_verification_blocks_application_bytes (state : Connection)
    (chain hostname : Bool) (stage : state.stage = .verifying)
    (bad : chain = false ∨ hostname = false) :
    canSend (advance state (.verification chain hostname)) = false := by
  cases chain <;> cases hostname <;> simp_all [canSend, advance]

theorem failure_is_terminal (state : Connection) (event : Event)
    (h : state.stage = .failed) : advance state event = state := by
  simp [advance, h]

theorem failure_stays_terminal_through_retries (state : Connection) (events : List Event)
    (h : state.stage = .failed) : events.foldl advance state = state := by
  induction events with
  | nil => rfl
  | cons event rest ih => simp [List.foldl_cons, failure_is_terminal state event h, ih]

end JadeCollab.Transport
