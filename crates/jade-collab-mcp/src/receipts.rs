//! Session-local edit outcomes retained independently of the SDK event drain.
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};

pub(crate) const RECEIPTS_PER_SESSION: usize = 64;

#[derive(Clone, Debug)]
pub(crate) enum Outcome {
    Invalid(String),
    Submitted {
        tickets: Vec<u64>,
        refusal: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Applied,
    Rejected(String),
}

#[derive(Debug)]
pub(crate) struct Receipt {
    outcomes: Vec<Outcome>,
    answers: BTreeMap<u64, Option<Verdict>>,
    connection_error: Option<String>,
}

impl Receipt {
    pub(crate) fn new(outcomes: Vec<Outcome>) -> Self {
        let answers = outcomes
            .iter()
            .flat_map(|o| match o {
                Outcome::Submitted { tickets, .. } => tickets.as_slice(),
                Outcome::Invalid(_) => &[],
            })
            .map(|&ticket| (ticket, None))
            .collect();
        Self {
            outcomes,
            answers,
            connection_error: None,
        }
    }

    pub(crate) fn complete(&self) -> bool {
        self.answers.values().all(Option::is_some)
    }

    pub(crate) fn through(&self) -> Option<u64> {
        self.answers.last_key_value().map(|(ticket, _)| *ticket)
    }

    pub(crate) fn render(&self) -> Value {
        let results: Vec<_> = self
            .outcomes
            .iter()
            .enumerate()
            .map(|(index, outcome)| match outcome {
                Outcome::Invalid(error) => json!({"index":index,"status":"invalid","error":error}),
                Outcome::Submitted { tickets, refusal } => {
                    let refused = refusal.as_deref().or_else(|| {
                        tickets.iter().find_map(|t| match self.answers.get(t) {
                            Some(Some(Verdict::Rejected(reason))) => Some(reason.as_str()),
                            _ => None,
                        })
                    });
                    if let Some(reason) = refused {
                        json!({"index":index,"status":"rejected","reason":reason})
                    } else if tickets
                        .iter()
                        .all(|t| matches!(self.answers.get(t), Some(Some(Verdict::Applied))))
                    {
                        json!({"index":index,"status":"applied"})
                    } else {
                        json!({"index":index,"status":"pending"})
                    }
                }
            })
            .collect();
        let mut value = json!({"results":results,"complete":self.complete(),"unresolved":self.answers.values().filter(|a|a.is_none()).count()});
        if let Some(error) = &self.connection_error {
            value["connection_error"] = json!(error);
        }
        value
    }
}

pub(crate) struct ReceiptCache {
    capacity: usize,
    entries: VecDeque<(String, Receipt)>,
}

impl Default for ReceiptCache {
    fn default() -> Self {
        Self {
            capacity: RECEIPTS_PER_SESSION,
            entries: VecDeque::new(),
        }
    }
}

impl ReceiptCache {
    pub(crate) fn has_room(&self) -> bool {
        self.entries.len() < self.capacity || self.entries.iter().any(|(_, r)| r.complete())
    }

    /// Never evicts an unanswered receipt. Check capacity before submitting.
    pub(crate) fn insert(&mut self, token: String, receipt: Receipt) -> Result<(), Receipt> {
        if self.entries.len() >= self.capacity {
            let Some(index) = self.entries.iter().position(|(_, r)| r.complete()) else {
                return Err(receipt);
            };
            self.entries.remove(index);
        }
        self.entries.push_back((token, receipt));
        Ok(())
    }

    pub(crate) fn get(&self, token: &str) -> Option<&Receipt> {
        self.entries
            .iter()
            .find(|(id, _)| id == token)
            .map(|(_, r)| r)
    }

    pub(crate) fn observe(&mut self, answers: Vec<(u64, Verdict)>) {
        for (ticket, verdict) in answers {
            for (_, receipt) in &mut self.entries {
                if let Some(answer @ None) = receipt.answers.get_mut(&ticket) {
                    *answer = Some(verdict.clone());
                }
            }
        }
    }

    pub(crate) fn disconnected(&mut self, error: String) {
        for (_, receipt) in &mut self.entries {
            if !receipt.complete() {
                receipt.connection_error = Some(error.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    struct Reference {
        token: String,
        // invalid, local refusal, tickets
        edits: Vec<(bool, bool, Vec<u64>)>,
        disconnected: bool,
    }

    impl Reference {
        fn complete(&self, answers: &BTreeMap<u64, bool>) -> bool {
            self.edits
                .iter()
                .flat_map(|(_, _, tickets)| tickets)
                .all(|t| answers.contains_key(t))
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        #[test]
        fn delayed_duplicate_and_foreign_verdicts_eviction_and_disconnects_match_the_model(
            steps in prop::collection::vec((0u8..8,any::<u64>()),1..120),
        ) {
            let mut cache = ReceiptCache {capacity:4,entries:VecDeque::new()};
            let mut model: VecDeque<Reference> = VecDeque::new();
            let mut answers: BTreeMap<u64,bool> = BTreeMap::new();
            let mut issued = std::collections::BTreeSet::new();
            let mut history = Vec::new();
            let mut next_ticket = 1u64;
            for (step,(kind,bits)) in steps.into_iter().enumerate() {
                match kind {
                    0..=2 => {
                        let token = format!("receipt-{step}");
                        let mut outcomes = Vec::new();
                        let mut edits = Vec::new();
                        let mut end = next_ticket;
                        for index in 0..1+bits as usize%4 {
                            let b = bits.rotate_right(index as u32*9);
                            let invalid = b%5 == 0;
                            let refusal = !invalid && b%7 == 0;
                            let tickets: Vec<_> = if invalid {vec![]} else {(0..b%3).map(|_|{let ticket=end;end+=1;ticket}).collect()};
                            outcomes.push(if invalid {Outcome::Invalid("invalid".into())} else {Outcome::Submitted {tickets:tickets.clone(),refusal:refusal.then(||"local refusal".into())}});
                            edits.push((invalid,refusal,tickets));
                        }
                        let evict = model.iter().position(|r|r.complete(&answers));
                        let permitted = model.len()<4 || evict.is_some();
                        prop_assert_eq!(cache.has_room(),permitted);
                        prop_assert_eq!(cache.insert(token.clone(),Receipt::new(outcomes)).is_ok(),permitted);
                        if permitted {
                            if model.len()==4 {model.remove(evict.unwrap());}
                            issued.extend(next_ticket..end);
                            next_ticket=end;
                            model.push_back(Reference {token:token.clone(),edits,disconnected:false});
                        }
                        history.push(token);
                    }
                    3..=5 => {
                        let ticket = if kind==5 {next_ticket+10} else {1+bits%next_ticket.max(1)};
                        let value = bits&1 == 0;
                        cache.observe(vec![(ticket,if value {Verdict::Applied} else {Verdict::Rejected("server refusal".into())})]);
                        if issued.contains(&ticket) {answers.entry(ticket).or_insert(value);}
                    }
                    6 => {
                        cache.disconnected("offline".into());
                        for receipt in &mut model {if !receipt.complete(&answers) {receipt.disconnected=true;}}
                    }
                    _ => {},
                }
                prop_assert!(cache.entries.len()<=4);
                prop_assert_eq!(cache.entries.len(),model.len());
                for token in &history {
                    let expected = model.iter().find(|r|&r.token==token);
                    prop_assert_eq!(cache.get(token).is_some(),expected.is_some());
                }
                for expected in &model {
                    let actual = cache.get(&expected.token).unwrap().render();
                    prop_assert_eq!(actual["complete"].as_bool(),Some(expected.complete(&answers)));
                    let unknown = expected.edits.iter().flat_map(|(_,_,t)|t).filter(|t|!answers.contains_key(t)).count();
                    prop_assert_eq!(actual["unresolved"].as_u64(),Some(unknown as u64));
                    prop_assert_eq!(actual.get("connection_error").is_some(),expected.disconnected);
                    for (index,(invalid,refusal,tickets)) in expected.edits.iter().enumerate() {
                        let status = if *invalid {"invalid"} else if *refusal || tickets.iter().any(|t|answers.get(t)==Some(&false)) {"rejected"}
                            else if tickets.iter().all(|t|answers.contains_key(t)) {"applied"} else {"pending"};
                        prop_assert_eq!(actual["results"][index]["status"].as_str(),Some(status));
                    }
                }
            }
        }
    }
}
