//! Whether to report at all.
//!
//! Reporting is opt-*out*: absent a decision, it is on. That makes the defaults load-bearing, so
//! this module exists to make them one testable function rather than a boolean checked in four
//! places.
//!
//! The structural part of consent is not here, though — it is that nothing constructs a transport
//! until [`Consent::resolve`] has returned [`Consent::Granted`]. A sink that exists and checks a
//! flag before each send is one missing check away from reporting anyway; a sink that does not
//! exist cannot.

use std::collections::BTreeMap;

/// The standard opt-out signal. Honoured because an agent runs on someone else's servers, where a
/// Pessimal-specific setting is not where they would think to look.
pub const DO_NOT_TRACK: &str = "DO_NOT_TRACK";

/// Pessimal's own override, for a host where `DO_NOT_TRACK` would be too broad a brush.
pub const OPT_OUT_VAR: &str = "PESSIMAL_USAGE_REPORTING";

/// Set by essentially every CI provider. Build agents are not users, and their volume would swamp
/// the signal from real ones.
pub const CI: &str = "CI";

/// The answer, and — when it is no — why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    Granted,
    Denied(DenialReason),
}

/// Why reporting is off. Recorded so a diagnostics screen can say which switch is the one in
/// effect; a user who turned it off in Settings and still sees it off because of `CI=true` would
/// otherwise have no way to tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenialReason {
    /// The user or operator turned it off.
    OptedOut,
    /// `DO_NOT_TRACK` is set to something other than `0`.
    DoNotTrack,
    /// Running under CI.
    ContinuousIntegration,
    /// The build carries no destination, which is every build except an official release.
    NoDestination,
}

impl Consent {
    /// Resolves every signal. Order matters only for which reason is reported, not for the answer:
    /// any one denial is decisive.
    ///
    /// `opted_out` is the user's own setting, which on Apple platforms comes from a store key
    /// deliberately outside the one a "reset connection" clears. `has_destination` is whether the
    /// build was given an endpoint and credential at all.
    #[must_use]
    pub fn resolve(opted_out: bool, has_destination: bool, env: &BTreeMap<String, String>) -> Self {
        if !has_destination {
            return Self::Denied(DenialReason::NoDestination);
        }
        if opted_out || env_says_off(env, OPT_OUT_VAR) {
            return Self::Denied(DenialReason::OptedOut);
        }
        if env_is_truthy(env, DO_NOT_TRACK) {
            return Self::Denied(DenialReason::DoNotTrack);
        }
        if env_is_truthy(env, CI) {
            return Self::Denied(DenialReason::ContinuousIntegration);
        }
        Self::Granted
    }

    #[must_use]
    pub fn is_granted(self) -> bool {
        matches!(self, Self::Granted)
    }
}

/// `DO_NOT_TRACK=1` and `CI=true` are the spellings in the wild, and `DO_NOT_TRACK=0` explicitly
/// means "do track". Anything else present and non-empty is treated as set, because a user who
/// typed `DO_NOT_TRACK=yes` meant it.
fn env_is_truthy(env: &BTreeMap<String, String>, name: &str) -> bool {
    match env.get(name).map(|v| v.trim().to_ascii_lowercase()) {
        None => false,
        Some(value) => !matches!(value.as_str(), "" | "0" | "false" | "no" | "off"),
    }
}

/// The inverse spelling, for a variable whose *presence* means on: `PESSIMAL_USAGE_REPORTING=off`.
fn env_says_off(env: &BTreeMap<String, String>, name: &str) -> bool {
    match env.get(name).map(|v| v.trim().to_ascii_lowercase()) {
        None => false,
        Some(value) => matches!(value.as_str(), "0" | "false" | "no" | "off" | "disabled"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn the_default_is_on_because_this_is_opt_out() {
        assert_eq!(Consent::resolve(false, true, &env(&[])), Consent::Granted);
    }

    #[test]
    fn a_build_with_no_destination_cannot_report() {
        // This is the guarantee that a `cargo build` from source, and the CI simulator build, never
        // phone home: there is nowhere for them to phone.
        assert_eq!(
            Consent::resolve(false, false, &env(&[])),
            Consent::Denied(DenialReason::NoDestination)
        );
    }

    #[test]
    fn the_users_own_switch_wins() {
        assert_eq!(
            Consent::resolve(true, true, &env(&[])),
            Consent::Denied(DenialReason::OptedOut)
        );
    }

    #[test]
    fn do_not_track_is_honoured_in_the_spellings_people_use() {
        for value in ["1", "true", "yes", "on", "TRUE"] {
            assert_eq!(
                Consent::resolve(false, true, &env(&[(DO_NOT_TRACK, value)])),
                Consent::Denied(DenialReason::DoNotTrack),
                "DO_NOT_TRACK={value}"
            );
        }
    }

    #[test]
    fn do_not_track_set_to_zero_means_do_track() {
        // The spec is explicit about this, and getting it backwards would be the kind of bug that
        // looks like working code.
        for value in ["0", "false", "no", "off", ""] {
            assert_eq!(
                Consent::resolve(false, true, &env(&[(DO_NOT_TRACK, value)])),
                Consent::Granted,
                "DO_NOT_TRACK={value}"
            );
        }
    }

    #[test]
    fn ci_is_not_a_user() {
        assert_eq!(
            Consent::resolve(false, true, &env(&[(CI, "true")])),
            Consent::Denied(DenialReason::ContinuousIntegration)
        );
    }

    #[test]
    fn pessimals_own_variable_turns_it_off_without_do_not_track() {
        for value in ["off", "0", "false", "no", "disabled"] {
            assert_eq!(
                Consent::resolve(false, true, &env(&[(OPT_OUT_VAR, value)])),
                Consent::Denied(DenialReason::OptedOut),
                "{OPT_OUT_VAR}={value}"
            );
        }
        // Anything else leaves it on, including the obvious affirmative.
        assert_eq!(
            Consent::resolve(false, true, &env(&[(OPT_OUT_VAR, "on")])),
            Consent::Granted
        );
    }

    #[test]
    fn a_missing_destination_outranks_every_other_reason() {
        // Reported first so a diagnostics screen says "this build cannot report" rather than
        // blaming a switch the user did not touch.
        assert_eq!(
            Consent::resolve(true, false, &env(&[(DO_NOT_TRACK, "1"), (CI, "true")])),
            Consent::Denied(DenialReason::NoDestination)
        );
    }
}
