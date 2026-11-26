//! Randomness without a crate, from std's randomly keyed SipHash.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

/// A pseudo-random number in `[0, 1)`, good enough for jitter. Hashes a
/// counter with std's randomly keyed SipHash instead of pulling in a crate.
pub(crate) fn random_fraction() -> f64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    (hasher.finish() >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_fraction_is_in_unit_interval_and_varies() {
        let values: Vec<f64> = (0..100).map(|_| random_fraction()).collect();
        assert!(values.iter().all(|v| (0.0..1.0).contains(v)));
        assert!(values.windows(2).any(|w| w[0] != w[1]));
    }
}
