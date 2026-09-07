//! Liveness derived from heartbeat age.
//!
//! Pessimal polls; it is never pushed to. A host is therefore judged by how old its most recent
//! heartbeat is relative to the interval the agent claims to beat at. Two thresholds rather than
//! one so a single missed export does not read as an outage.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// How alive a host looks right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Liveness {
    /// Beating on schedule.
    Alive,
    /// Late, but not late enough to call it. Usually a dropped export or a slow backend.
    Stale,
    /// Silent long enough to treat as down.
    Down,
    /// No heartbeat at all in the queried window — a host that never reported, or one whose
    /// history has aged out. Distinct from [`Liveness::Down`], which is a positive judgement.
    Unknown,
}

impl Liveness {
    /// Whether this state should raise attention in the UI.
    #[must_use]
    pub fn is_degraded(self) -> bool {
        matches!(self, Self::Stale | Self::Down)
    }
}

/// The rule for turning heartbeat age into [`Liveness`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "LivenessPolicyWire")]
pub struct LivenessPolicy {
    heartbeat_interval: Duration,
    stale_after_intervals: u32,
    down_after_intervals: u32,
}

/// `interval * multiplier`, saturating instead of panicking.
///
/// `Duration * i32` is `checked_mul(...).expect(...)`, and both callers are `#[must_use]` getters
/// with no `Result` to return — a panic in one of them crosses UniFFI as an app crash rather than
/// as an error. [`LivenessPolicy::new`] checks only that the interval is positive and that the
/// multipliers are non-zero and ordered, so a policy with a decades-long interval and a
/// `u32::MAX` multiplier passes validation, round-trips through JSON, and used to blow up in its
/// own accessor. Saturating is monotone-correct for every comparison a caller makes: a threshold
/// this large is already further away than any heartbeat age could reach, so it answers the same
/// question the exact value would.
///
/// Note what this does *not* promise. `checked_mul` guards only against overflowing `i64`
/// seconds, so the product it returns can still exceed `TimeDelta::MAX` — a `Duration` that is
/// well-formed enough to compare against but far outside what a timestamp can represent. Callers
/// that go on to add a threshold to an instant must bound it themselves;
/// `pessimal_client_core`'s `MAX_TUNING_DURATION` is where that happens.
fn scaled(interval: Duration, multiplier: u32) -> Duration {
    interval
        .checked_mul(i32::try_from(multiplier).unwrap_or(i32::MAX))
        .unwrap_or(Duration::MAX)
}

impl LivenessPolicy {
    /// # Errors
    /// Returns [`CoreError::InvalidRule`] if the interval is not positive, if either multiplier is
    /// zero, or if `stale_after_intervals >= down_after_intervals`.
    pub fn new(
        heartbeat_interval: Duration,
        stale_after_intervals: u32,
        down_after_intervals: u32,
    ) -> Result<Self, CoreError> {
        if heartbeat_interval <= Duration::zero() {
            return Err(CoreError::InvalidRule(
                "heartbeat interval must be positive".to_owned(),
            ));
        }
        if stale_after_intervals == 0 || down_after_intervals == 0 {
            return Err(CoreError::InvalidRule(
                "interval multipliers must be at least 1".to_owned(),
            ));
        }
        if stale_after_intervals >= down_after_intervals {
            return Err(CoreError::InvalidRule(format!(
                "stale threshold ({stale_after_intervals}) must be below down threshold ({down_after_intervals})"
            )));
        }
        Ok(Self {
            heartbeat_interval,
            stale_after_intervals,
            down_after_intervals,
        })
    }

    #[must_use]
    pub fn heartbeat_interval(&self) -> Duration {
        self.heartbeat_interval
    }

    /// Age beyond which a host is [`Liveness::Stale`].
    #[must_use]
    pub fn stale_threshold(&self) -> Duration {
        scaled(self.heartbeat_interval, self.stale_after_intervals)
    }

    /// Age beyond which a host is [`Liveness::Down`].
    #[must_use]
    pub fn down_threshold(&self) -> Duration {
        scaled(self.heartbeat_interval, self.down_after_intervals)
    }

    /// Judges a host from the timestamp of its most recent heartbeat.
    ///
    /// A heartbeat timestamped in the future is treated as [`Liveness::Alive`]: modest clock skew
    /// between an agent and the backend is normal and is not evidence of an outage.
    #[must_use]
    pub fn evaluate(&self, last_heartbeat: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Liveness {
        let Some(last) = last_heartbeat else {
            return Liveness::Unknown;
        };
        let age = now - last;
        if age <= self.stale_threshold() {
            Liveness::Alive
        } else if age <= self.down_threshold() {
            Liveness::Stale
        } else {
            Liveness::Down
        }
    }
}

/// The only shape [`LivenessPolicy`] deserialises through, so a persisted or synced value
/// cannot bypass [`LivenessPolicy::new`].
///
/// Without this, restoring `{stale_after_intervals: 9, down_after_intervals: 1}` from disk yields a
/// policy that calls a host down before it calls it stale — the liveness model inverted, with no
/// error anywhere.
#[derive(Deserialize)]
struct LivenessPolicyWire {
    heartbeat_interval: Duration,
    stale_after_intervals: u32,
    down_after_intervals: u32,
}

impl TryFrom<LivenessPolicyWire> for LivenessPolicy {
    type Error = CoreError;

    fn try_from(wire: LivenessPolicyWire) -> Result<Self, Self::Error> {
        Self::new(
            wire.heartbeat_interval,
            wire.stale_after_intervals,
            wire.down_after_intervals,
        )
    }
}

impl Default for LivenessPolicy {
    /// A 30-second beat, stale at 3 missed intervals, down at 5.
    ///
    /// Three rather than two because a heartbeat's timestamp comes back quantised to the start of
    /// the query bucket it fell in, so a perfectly healthy host's *measured* age can already be a
    /// full interval older than its true age. At two intervals that lands exactly on the stale
    /// threshold and the host flickers; three leaves an interval of headroom for the quantisation
    /// plus poll latency.
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::seconds(30),
            stale_after_intervals: 3,
            down_after_intervals: 5,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_760_000_000 + secs, 0).expect("valid timestamp")
    }

    fn policy() -> LivenessPolicy {
        LivenessPolicy::new(Duration::seconds(30), 2, 5).expect("valid policy")
    }

    #[test]
    fn thresholds_are_multiples_of_the_interval() {
        let policy = policy();
        assert_eq!(policy.stale_threshold(), Duration::seconds(60));
        assert_eq!(policy.down_threshold(), Duration::seconds(150));
    }

    #[test]
    fn thresholds_saturate_rather_than_panic_on_an_absurd_multiplier() {
        // `new` checks only that the interval is positive and the multipliers are non-zero and
        // ordered, so this policy is legal and round-trips through JSON. Its thresholds overflow
        // `i64` seconds, which used to panic inside `#[must_use]` getters that have no `Result` to
        // return — and a panic crosses UniFFI as an app crash.
        let absurd =
            LivenessPolicy::new(Duration::days(100_000), 3, u32::MAX).expect("valid policy");

        assert_eq!(absurd.down_threshold(), Duration::MAX);
        assert_eq!(
            absurd.evaluate(Some(at(0)), at(1_000_000)),
            Liveness::Alive,
            "a saturated threshold is further away than any age, which is the answer the exact \
             value would give too"
        );

        // Below the `i64`-seconds cliff, `checked_mul` succeeds and hands back a product larger
        // than `TimeDelta::MAX`. No panic, but nothing a timestamp could hold either — which is
        // why the client crate bounds these thresholds rather than trusting them.
        let merely_enormous =
            LivenessPolicy::new(Duration::days(365), 3, u32::MAX).expect("valid policy");
        assert!(merely_enormous.down_threshold() > Duration::days(365));
    }

    #[test]
    fn a_recent_heartbeat_is_alive() {
        assert_eq!(policy().evaluate(Some(at(0)), at(30)), Liveness::Alive);
    }

    #[test]
    fn the_stale_boundary_is_inclusive() {
        assert_eq!(policy().evaluate(Some(at(0)), at(60)), Liveness::Alive);
        assert_eq!(policy().evaluate(Some(at(0)), at(61)), Liveness::Stale);
    }

    #[test]
    fn the_down_boundary_is_inclusive() {
        assert_eq!(policy().evaluate(Some(at(0)), at(150)), Liveness::Stale);
        assert_eq!(policy().evaluate(Some(at(0)), at(151)), Liveness::Down);
    }

    #[test]
    fn no_heartbeat_is_unknown_not_down() {
        assert_eq!(policy().evaluate(None, at(0)), Liveness::Unknown);
    }

    #[test]
    fn clock_skew_into_the_future_reads_as_alive() {
        assert_eq!(policy().evaluate(Some(at(60)), at(0)), Liveness::Alive);
    }

    #[test]
    fn stale_and_down_are_degraded_but_unknown_is_not() {
        assert!(Liveness::Stale.is_degraded());
        assert!(Liveness::Down.is_degraded());
        assert!(!Liveness::Alive.is_degraded());
        assert!(!Liveness::Unknown.is_degraded());
    }

    #[test]
    fn rejects_a_non_positive_interval() {
        assert!(LivenessPolicy::new(Duration::zero(), 2, 5).is_err());
        assert!(LivenessPolicy::new(Duration::seconds(-1), 2, 5).is_err());
    }

    #[test]
    fn rejects_thresholds_that_are_not_ordered() {
        assert!(LivenessPolicy::new(Duration::seconds(30), 5, 2).is_err());
        assert!(LivenessPolicy::new(Duration::seconds(30), 3, 3).is_err());
    }

    #[test]
    fn rejects_zero_multipliers() {
        assert!(LivenessPolicy::new(Duration::seconds(30), 0, 5).is_err());
        assert!(LivenessPolicy::new(Duration::seconds(30), 2, 0).is_err());
    }

    #[test]
    fn the_default_policy_leaves_headroom_for_bucket_quantisation() {
        let default = LivenessPolicy::default();
        let interval = default.heartbeat_interval();

        // Worst case for a healthy host: a full interval since its last beat, plus up to another
        // full interval of quantisation from the query bucket that beat landed in.
        let worst_case_measured_age = interval * 2;
        assert!(
            worst_case_measured_age < default.stale_threshold(),
            "a healthy host would flicker stale"
        );
        assert_eq!(
            default.evaluate(Some(at(0)), at(0) + worst_case_measured_age),
            Liveness::Alive
        );
    }
}
