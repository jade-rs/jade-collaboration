//! Limits on presence: how large one update may be and how often a
//! connection may send one.

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

    fn arrivals() -> impl Strategy<Value = Vec<u64>> {
        let gap = prop_oneof![
            4 => 0..1_000u64,
            4 => 0..200_000_000u64,
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

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// The bucket lets through exactly what the reference does, for
        /// any limits and any arrival times.
        #[test]
        fn the_bucket_agrees_with_the_reference(
            burst in 1..8u32,
            per_second in prop_oneof![Just(0u32), 1..30u32, Just(1_000_000u32)],
            arrivals in arrivals(),
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
