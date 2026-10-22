//! Short registry transitions, separate from document loading and actor I/O.
use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

enum Room<T> {
    Opening(Arc<()>),
    Ready(T),
}

pub(crate) struct Registry<T> {
    entries: Mutex<BTreeMap<String, Room<T>>>,
}

impl<T> Default for Registry<T> {
    fn default() -> Self {
        Self {
            entries: Mutex::default(),
        }
    }
}

impl<T> Registry<T> {
    /// Reserves an absent name without holding the mutex during initialization.
    pub(crate) fn reserve(&self, name: &str) -> Option<Reservation<'_, T>> {
        let mut entries = self.entries.lock().unwrap();
        let Entry::Vacant(entry) = entries.entry(name.to_owned()) else {
            return None;
        };
        let owner = Arc::new(());
        entry.insert(Room::Opening(owner.clone()));
        Some(Reservation {
            registry: self,
            name: name.to_owned(),
            owner,
        })
    }

    /// Only cheap metadata projections belong inside these closures.
    pub(crate) fn with_ready<R>(&self, name: &str, project: impl FnOnce(&T) -> R) -> Option<R> {
        match self.entries.lock().unwrap().get(name) {
            Some(Room::Ready(value)) => Some(project(value)),
            _ => None,
        }
    }

    pub(crate) fn list<R>(&self, mut project: impl FnMut(&str, &T) -> R) -> Vec<R> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(name, entry)| match entry {
                Room::Ready(value) => Some(project(name, value)),
                Room::Opening(_) => None,
            })
            .collect()
    }
}

/// Dropping an unsuccessful initializer releases only its own claim, including
/// during unwinding. A completed actor becomes visible in one short transition.
pub(crate) struct Reservation<'a, T> {
    registry: &'a Registry<T>,
    name: String,
    owner: Arc<()>,
}

impl<T> Reservation<'_, T> {
    pub(crate) fn publish(self, value: T) -> Result<(), T> {
        let mut entries = self.registry.entries.lock().unwrap();
        if matches!(entries.get(&self.name),Some(Room::Opening(owner)) if Arc::ptr_eq(owner,&self.owner))
        {
            entries.insert(self.name.clone(), Room::Ready(value));
            Ok(())
        } else {
            Err(value)
        }
    }
}

impl<T> Drop for Reservation<'_, T> {
    fn drop(&mut self) {
        let mut entries = self.registry.entries.lock().unwrap();
        if matches!(entries.get(&self.name),Some(Room::Opening(owner)) if Arc::ptr_eq(owner,&self.owner))
        {
            entries.remove(&self.name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        #[test]
        fn interleaved_claims_publications_and_aborts_match_the_reference(
            steps in prop::collection::vec((0u8..7,0usize..5,any::<u64>()),1..90),
        ) {
            let registry = Registry::default();
            let mut claims = BTreeMap::new();
            // None = vacant, Some(None) = opening, Some(Some(value)) = ready.
            let mut reference = [None;5];
            for (kind,key,value) in steps {
                let name = key.to_string();
                match kind {
                    0|1 => {
                        let claim = registry.reserve(&name);
                        prop_assert_eq!(claim.is_some(),reference[key].is_none());
                        if let Some(claim) = claim {claims.insert(key,claim);reference[key]=Some(None);}
                    }
                    2 => {
                        if let Some(claim) = claims.remove(&key) {
                            claim.publish(value).unwrap(); reference[key]=Some(Some(value));
                        }
                    }
                    3 => {if claims.remove(&key).is_some() {reference[key]=None;}}
                    4 => {
                        // An initializer reports failure: its guard releases
                        // the name before a retry may begin.
                        let claim = registry.reserve(&name);
                        prop_assert_eq!(claim.is_some(),reference[key].is_none());
                        drop(claim);
                    }
                    5 => {
                        if let Some(claim) = registry.reserve(&name) {
                            prop_assert!(reference[key].is_none());
                            claim.publish(value).unwrap(); reference[key]=Some(Some(value));
                        } else {prop_assert!(reference[key].is_some());}
                    }
                    _ => {},
                }
                let expected: Vec<_> = reference.iter().enumerate().filter_map(|(k,v)|v.flatten().map(|v|(k.to_string(),v))).collect();
                prop_assert_eq!(registry.list(|name,value|(name.to_owned(),*value)),expected);
                for (k,v) in reference.iter().enumerate() {prop_assert_eq!(registry.with_ready(&k.to_string(),|v|*v),v.flatten());}
            }
            drop(claims);
            for (k,state) in reference.iter().enumerate() {
                let claim = registry.reserve(&k.to_string());
                prop_assert_eq!(claim.is_some(),state.is_none() || *state == Some(None));
            }
        }
    }
}
