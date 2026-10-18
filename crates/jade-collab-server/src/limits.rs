//! Limits on presence: how large one update may be and how often a
//! connection may send one. Updates over either are dropped and the sender
//! is told; operations are never limited.

use std::time::Duration;

use serde_json::Value;

/// What one connection may share as presence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresenceLimits {
    /// The largest `state`, in bytes of the JSON relayed to the others.
    pub max_bytes: usize,
    /// How many updates a connection may send at once; 0 turns presence off.
    pub burst: u32,
    /// How many updates per second it may keep sending after that; 0 for
    /// none once the burst is spent.
    pub per_second: u32,
}

impl PresenceLimits {
    /// No limits at all.
    pub const UNLIMITED: Self = Self {
        max_bytes: usize::MAX,
        burst: u32::MAX,
        per_second: u32::MAX,
    };

    /// Whether `state` is small enough to relay; if not, why.
    pub(crate) fn check_size(&self, state: &Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(state)
            .expect("JSON values serialize")
            .len();
        if bytes <= self.max_bytes {
            Ok(())
        } else {
            Err(format!(
                "presence of {bytes} bytes is over the limit of {} bytes",
                self.max_bytes
            ))
        }
    }

    pub(crate) fn rate_message(&self) -> String {
        format!(
            "presence is limited to {} updates at once and {} per second",
            self.burst, self.per_second
        )
    }
}

impl Default for PresenceLimits {
    /// Plenty for cursors and selections at typing speed.
    fn default() -> Self {
        Self {
            max_bytes: 16 * 1024,
            burst: 40,
            per_second: 20,
        }
    }
}

/// One update's worth of credit: credit is kept in billionths of an update,
/// so that refilling at `per_second` updates per second adds exactly
/// `per_second` of them per nanosecond and nothing is rounded.
const UPDATE: u128 = 1_000_000_000;

/// A token bucket: `burst` updates of credit, refilled at `per_second`, one
/// spent per update let through. Starts full.
pub(crate) struct Bucket {
    capacity: u128,
    per_second: u128,
    credit: u128,
    /// When credit was last brought up to date, since the bucket started.
    last: Duration,
}

impl Bucket {
    pub fn new(limits: PresenceLimits) -> Self {
        let capacity = u128::from(limits.burst) * UPDATE;
        Self {
            capacity,
            per_second: u128::from(limits.per_second),
            credit: capacity,
            last: Duration::ZERO,
        }
    }

    /// Whether an update arriving `at` (since the bucket started; never
    /// earlier than the previous one) is let through.
    pub fn allow(&mut self, at: Duration) -> bool {
        let elapsed = at.saturating_sub(self.last).as_nanos();
        self.last = self.last.max(at);
        self.credit = self
            .credit
            .saturating_add(elapsed.saturating_mul(self.per_second))
            .min(self.capacity);
        if self.credit >= UPDATE {
            self.credit -= UPDATE;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::time::Duration;

    /// The generic cell rate algorithm (virtual scheduling), in exact
    /// integers: an independent statement of "`burst` at once, then
    /// `per_second`". Time is scaled by `per_second` so that one update
    /// takes `NANOS` of it.
    fn reference(burst: u32, per_second: u32, arrivals: &[u64]) -> Vec<bool> {
        const NANOS: i128 = 1_000_000_000;
        let tolerance = (i128::from(burst) - 1) * NANOS;
        let mut theoretical_arrival: i128 = 0;
        arrivals
            .iter()
            .map(|&at| {
                let now = i128::from(at) * i128::from(per_second);
                let conforms = theoretical_arrival <= now + tolerance;
                if conforms {
                    theoretical_arrival = theoretical_arrival.max(now) + NANOS;
                }
                conforms
            })
            .collect()
    }

    /// Arrival times, with gaps of nothing, of anything, and of right at,
    /// just before or just after a whole number of refills at `per_second`
    /// — where an off-by-one would show.
    fn arrivals(per_second: u32) -> impl Strategy<Value = Vec<u64>> {
        let interval = 1_000_000_000 / u64::from(per_second.max(1));
        let refill = (1..4u64, 0..3u64).prop_map(move |(updates, offset)| {
            (updates * 1_000_000_000 / u64::from(per_second.max(1)) + offset).saturating_sub(1)
        });
        let gap = prop_oneof![
            3 => Just(0u64),
            2 => 0..interval.max(2),
            4 => refill,
            1 => 0..5_000_000_000u64,
        ];
        prop::collection::vec(gap, 0..80).prop_map(|gaps| {
            gaps.into_iter()
                .scan(0u64, |at, gap| {
                    *at += gap;
                    Some(*at)
                })
                .collect()
        })
    }

    fn limits_and_arrivals() -> impl Strategy<Value = (u32, u32, Vec<u64>)> {
        (
            1..8u32,
            prop_oneof![Just(0u32), 1..30u32, Just(1_000_000u32)],
        )
            .prop_flat_map(|(burst, per_second)| {
                (Just(burst), Just(per_second), arrivals(per_second))
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// The bucket lets through exactly what the reference does, for
        /// any limits and any arrival times.
        #[test]
        fn the_bucket_agrees_with_the_reference(
            (burst, per_second, arrivals) in limits_and_arrivals(),
        ) {
            let limits = PresenceLimits { max_bytes: usize::MAX, burst, per_second };
            let mut bucket = Bucket::new(limits);
            let allowed: Vec<bool> = arrivals
                .iter()
                .map(|&at| bucket.allow(Duration::from_nanos(at)))
                .collect();
            prop_assert_eq!(allowed, reference(burst, per_second, &arrivals));
        }
    }

    #[test]
    fn edge_limits() {
        let at = |s: u64| Duration::from_secs(s);
        // No burst: presence is off.
        let mut off = Bucket::new(PresenceLimits {
            burst: 0,
            ..PresenceLimits::UNLIMITED
        });
        assert!((0..5).all(|s| !off.allow(at(s * 1000))));
        // Unlimited lets everything through, at any instant.
        let mut open = Bucket::new(PresenceLimits::UNLIMITED);
        assert!((0..10_000).all(|_| open.allow(at(0))));
        // A long silence refills the burst, never more.
        let mut bucket = Bucket::new(PresenceLimits {
            burst: 2,
            per_second: 1,
            ..PresenceLimits::UNLIMITED
        });
        let allowed: Vec<bool> = [0, 0, 0, 100, 100, 100]
            .map(|s| bucket.allow(at(s)))
            .to_vec();
        assert_eq!(allowed, [true, true, false, true, true, false]);
    }

    #[test]
    fn sizes_are_measured_as_relayed_json() {
        let limits = PresenceLimits {
            max_bytes: 10,
            ..PresenceLimits::UNLIMITED
        };
        assert_eq!(limits.check_size(&serde_json::json!("12345678")), Ok(()));
        let error = limits
            .check_size(&serde_json::json!("123456789"))
            .unwrap_err();
        assert!(
            error.contains("11 bytes") && error.contains("10"),
            "{error}"
        );
    }
}
