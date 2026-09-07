//! Rule authoring: the one construction path, the re-check for a rule that came back from
//! storage, and the fingerprint that decides when an edited rule must forfeit its dwell.
//!
//! The fold never touches [`AlertRuleRepository`]. Rules reach it as a plain `Vec<AlertRule>`
//! snapshot inside `FleetConfig`, which is what keeps the reducer a pure function of three
//! values; the `async` wrappers here exist for the settings screen, the only caller that writes.
//!
//! [`AlertRule`]'s invariants hold in `AlertRule::new` and nowhere else — its fields are public
//! and it derives `Deserialize` — so a rule that has been round-tripped through storage or edited
//! field-by-field is unvalidated until [`validate_rule`] says otherwise. That is why the fold
//! re-checks every rule on its critical path rather than trusting the type.

use chrono::Duration;
use pessimal_core::{AlertRule, AlertRuleRepository, Comparator, HostSelector, MetricKind, Urn};

use crate::error::{ClientError, Result};
use crate::view::alertable;

/// The only construction path the apps use.
///
/// Wraps `AlertRule::new`, which mints a UUIDv7 URN as `pessimal::<environment>::alerts::rule::…`,
/// and applies the selector afterwards because `new` always starts at [`HostSelector::All`].
///
/// The environment is checked by `Urn::with_id` on every call, so a rule drafted against a
/// caller-supplied environment string is safe even when no `FleetConfig` guarded it first.
///
/// # Errors
/// [`ClientError::InvalidRule`] for an empty name, a non-finite threshold, a negative dwell, a
/// dwell above [`AlertRule::MAX_FOR_DURATION`], a non-alertable metric (`AgentHeartbeat`), or
/// `HostSelector::AnyOf(vec![])`, which matches nothing and would silently never fire.
/// [`ClientError::InvalidUrn`] if `environment` is empty or contains `::`.
pub fn draft_rule(
    environment: &str,
    name: &str,
    metric: MetricKind,
    comparator: Comparator,
    threshold: f64,
    for_duration: Duration,
    selector: HostSelector,
) -> Result<AlertRule> {
    check_fields(name, metric, threshold, for_duration, &selector)?;
    let rule = AlertRule::new(
        environment,
        name,
        metric,
        comparator,
        threshold,
        for_duration,
    )?;
    Ok(rule.with_selector(selector))
}

/// Re-checks what `AlertRule::new` enforces, on a rule that came back from storage.
///
/// A NaN threshold is the case that motivates it: every comparator returns `false` against NaN,
/// so the rule is not broken loudly — it simply never fires, for as long as it exists. The fold
/// calls this per rule and surfaces the message as `AlertView.invalid_reason` rather than
/// dropping the rule, because a rule that has vanished from the list is indistinguishable from
/// one that is quietly wrong.
///
/// # Errors
/// [`ClientError::InvalidRule`] for an empty name, a non-finite threshold, a negative dwell, a
/// dwell above [`AlertRule::MAX_FOR_DURATION`], a non-alertable metric, or an empty `AnyOf`
/// selector.
pub fn validate_rule(rule: &AlertRule) -> Result<()> {
    check_fields(
        &rule.name,
        rule.metric,
        rule.threshold,
        rule.for_duration(),
        &rule.selector,
    )
}

/// The single predicate behind both [`draft_rule`] and [`validate_rule`], so the same defect gets
/// the same message from both and a rule this crate drafts always survives its own validator.
fn check_fields(
    name: &str,
    metric: MetricKind,
    threshold: f64,
    for_duration: Duration,
    selector: &HostSelector,
) -> Result<()> {
    // `trim` rather than `is_empty`, matching `AlertRule::new`: a name of three spaces is an
    // unlabelled row in the rule list, not a name.
    if name.trim().is_empty() {
        return Err(ClientError::InvalidRule(
            "name must not be empty".to_owned(),
        ));
    }
    if !threshold.is_finite() {
        return Err(ClientError::InvalidRule(format!(
            "threshold {threshold} must be finite"
        )));
    }
    if for_duration < Duration::zero() {
        return Err(ClientError::InvalidRule(
            "for_duration must not be negative".to_owned(),
        ));
    }
    // `AlertRule::MAX_FOR_DURATION` rather than a literal, so the two enforcement points cannot
    // drift and a rule core accepts can never be one this validator flags. The bound is about
    // overflow, not UX: `AlertView::fires_at` adds this dwell to the instant the breach began,
    // and past the ceiling that addition leaves the range a timestamp can represent.
    if for_duration > AlertRule::MAX_FOR_DURATION {
        return Err(ClientError::InvalidRule(format!(
            "for_duration {}s exceeds the maximum dwell of {}s, past which the instant a rule \
             would fire is not representable",
            for_duration.num_seconds(),
            AlertRule::MAX_FOR_DURATION.num_seconds()
        )));
    }
    if !alertable(metric) {
        return Err(ClientError::InvalidRule(format!(
            "{} cannot carry an alert rule: a silent host is liveness's business",
            metric.display_name()
        )));
    }
    if matches!(selector, HostSelector::AnyOf(hosts) if hosts.is_empty()) {
        return Err(ClientError::InvalidRule(
            "an empty AnyOf selector matches no host, so the rule could never fire".to_owned(),
        ));
    }
    Ok(())
}

/// FNV-1a 64-bit, offset basis and prime.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// An FNV-1a accumulator, hand-rolled for the reason spelled out on [`rule_fingerprint`].
struct Fingerprint {
    hash: u64,
}

impl Fingerprint {
    fn start() -> Self {
        Self {
            hash: FNV_OFFSET_BASIS,
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.hash ^= u64::from(*byte);
            self.hash = self.hash.wrapping_mul(FNV_PRIME);
        }
    }

    fn tag(&mut self, tag: u8) {
        self.write(&[tag]);
    }

    /// Length-prefixed, because concatenation alone is ambiguous: without the prefix
    /// `AnyOf(["ab", "c"])` and `AnyOf(["a", "bc"])` hash identically and one of those edits
    /// would keep a dwell it has no right to. `u64` rather than `usize` so the value does not
    /// depend on the pointer width of the device that computed it.
    fn field(&mut self, bytes: &[u8]) {
        self.write(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
        self.write(bytes);
    }

    fn finish(self) -> u64 {
        self.hash
    }
}

/// A stable hash over the fields whose change makes an accumulated dwell meaningless.
///
/// Stored beside each evaluation; the fold replaces any evaluation whose stored fingerprint no
/// longer matches, so an edited rule starts from a fresh `NoData` instead of inheriting an old
/// `since`.
///
/// **Judged** — `metric`, `selector`, `comparator`, `threshold`, `for_duration`. Each changes
/// either what counts as a breach or how long a breach must hold, so the stored `since` was
/// measured against a question nobody is asking any more. Shortening `for_duration` on a
/// `Pending` rule is the sharp case: `since + for_duration` lands in the past and the rule fires
/// instantly on evidence gathered under the old dwell.
///
/// **Not judged**, deliberately:
/// - `name`, and prose generally — renaming an alert must not resolve it and re-fire it a dwell
///   later. The breach it is watching did not change.
/// - `enabled` — the fold's reconciliation drops the evaluation for a disabled rule outright, so
///   re-enabling already produces a fresh one. Hashing it would add a second, redundant path to
///   the same answer.
/// - `id` — it is the key the evaluation is stored under, not part of the rule's content. Two
///   rules with identical fields must fingerprint identically, or nothing could distinguish an
///   edit from a coincidence.
///
/// FNV-1a rather than `DefaultHasher`: this value is persisted inside `FleetState` JSON and
/// compared against one computed by a later build of the app. `DefaultHasher` is `SipHash` with
/// parameters the standard library reserves the right to change between releases, so a toolchain
/// bump would mismatch every stored fingerprint at once — every firing alert would silently drop
/// to `NoData` and re-fire a dwell later, on nothing but a rebuild.
#[must_use]
pub fn rule_fingerprint(rule: &AlertRule) -> u64 {
    let mut fingerprint = Fingerprint::start();
    fingerprint.field(rule.metric.otel_name().as_bytes());
    fingerprint.field(rule.comparator.symbol().as_bytes());
    fingerprint.write(&rule.threshold.to_bits().to_le_bytes());
    // Seconds and nanoseconds separately: `num_milliseconds` saturates on a large `Duration`,
    // which would map two different dwells onto one fingerprint.
    fingerprint.write(&rule.for_duration().num_seconds().to_le_bytes());
    fingerprint.write(&rule.for_duration().subsec_nanos().to_le_bytes());
    match &rule.selector {
        HostSelector::All => fingerprint.tag(0),
        HostSelector::Host(host) => {
            fingerprint.tag(1);
            fingerprint.field(host.as_str().as_bytes());
        }
        HostSelector::AnyOf(hosts) => {
            fingerprint.tag(2);
            fingerprint.write(&u64::try_from(hosts.len()).unwrap_or(u64::MAX).to_le_bytes());
            for host in hosts {
                fingerprint.field(host.as_str().as_bytes());
            }
        }
    }
    fingerprint.finish()
}

/// Every stored rule, invalid ones included.
///
/// Deliberately unfiltered: a rule that fails [`validate_rule`] is shown, flagged, and not
/// evaluated. Dropping it here would make it invisible in the settings screen, which is exactly
/// where someone could fix it.
///
/// # Errors
/// Propagates the repository's error.
pub async fn load_rules(repo: &dyn AlertRuleRepository) -> Result<Vec<AlertRule>> {
    Ok(repo.list().await?)
}

/// Persists the rule as it stands.
///
/// The rule goes to the repository by reference and is stored through its own `Serialize`. It is
/// never rebuilt with `AlertRule::new`, which would mint a fresh `Uuid::now_v7()` and orphan
/// every evaluation, every `delete` key and every `get` key that pointed at the old id — a bug
/// that presents to the user as "my alerts reset every launch".
///
/// # Errors
/// Propagates the repository's error.
pub async fn save_rule(repo: &dyn AlertRuleRepository, rule: &AlertRule) -> Result<()> {
    repo.save(rule).await?;
    Ok(())
}

/// # Errors
/// [`ClientError::RuleNotFound`] if no rule has that id, per the port's contract; otherwise
/// propagates the repository's error.
pub async fn delete_rule(repo: &dyn AlertRuleRepository, id: &Urn) -> Result<()> {
    repo.delete(id).await?;
    Ok(())
}

/// Parses a rule id that crossed the FFI boundary as a string, since `Urn` cannot cross it.
///
/// # Errors
/// [`ClientError::InvalidUrn`] unless `value` parses as a URN.
pub fn parse_rule_id(value: &str) -> Result<Urn> {
    Ok(value.parse::<Urn>()?)
}

#[cfg(test)]
mod tests {
    use futures::executor::block_on;
    use pessimal_core::HostId;

    use super::*;
    use crate::testing::InMemoryRuleRepository;

    /// A well-formed rule, drafted the way the app drafts one.
    fn drafted() -> AlertRule {
        draft_rule(
            "prod",
            "CPU hot",
            MetricKind::CpuUtilization,
            Comparator::GreaterThan,
            0.9,
            Duration::seconds(120),
            HostSelector::All,
        )
        .expect("valid rule")
    }

    #[test]
    fn a_saved_rule_reloads_with_an_identical_id() {
        let repo = InMemoryRuleRepository::new();
        let rule = drafted();

        block_on(save_rule(&repo, &rule)).expect("save");
        let reloaded = block_on(load_rules(&repo)).expect("load");

        assert_eq!(reloaded.len(), 1);
        assert_eq!(
            reloaded[0].id(),
            rule.id(),
            "a save that rebuilt the rule would mint a fresh UUIDv7 and orphan its evaluations"
        );
        assert_eq!(&reloaded[0], &rule);
    }

    #[test]
    fn draft_rule_rejects_an_empty_name_a_nan_threshold_and_a_negative_dwell() {
        let unnamed = draft_rule(
            "prod",
            "   ",
            MetricKind::CpuUtilization,
            Comparator::GreaterThan,
            0.9,
            Duration::seconds(120),
            HostSelector::All,
        );
        assert!(matches!(unnamed, Err(ClientError::InvalidRule(_))));

        let nan = draft_rule(
            "prod",
            "CPU hot",
            MetricKind::CpuUtilization,
            Comparator::GreaterThan,
            f64::NAN,
            Duration::seconds(120),
            HostSelector::All,
        );
        assert!(
            matches!(nan, Err(ClientError::InvalidRule(_))),
            "every comparator returns false against NaN, so the rule would never fire"
        );

        let backwards = draft_rule(
            "prod",
            "CPU hot",
            MetricKind::CpuUtilization,
            Comparator::GreaterThan,
            0.9,
            Duration::seconds(-1),
            HostSelector::All,
        );
        assert!(matches!(backwards, Err(ClientError::InvalidRule(_))));
    }

    #[test]
    fn draft_rule_and_validate_rule_reject_a_dwell_past_the_ceiling() {
        let with_dwell = |dwell| {
            draft_rule(
                "prod",
                "CPU hot",
                MetricKind::CpuUtilization,
                Comparator::GreaterThan,
                0.9,
                dwell,
                HostSelector::All,
            )
        };

        // The reproduction's value: a legal `Duration` that used to reach
        // `AlertView::fires_at` and panic on `since + for_duration`.
        assert!(matches!(
            with_dwell(Duration::seconds(10_000_000_000_000)),
            Err(ClientError::InvalidRule(_))
        ));
        assert!(matches!(
            with_dwell(AlertRule::MAX_FOR_DURATION + Duration::seconds(1)),
            Err(ClientError::InvalidRule(_))
        ));

        // The boundary is legal, and a rule this crate drafts must survive its own validator —
        // which is the whole reason both routes read the same constant.
        let at_the_ceiling = with_dwell(AlertRule::MAX_FOR_DURATION).expect("the bound is legal");
        assert!(validate_rule(&at_the_ceiling).is_ok());
    }

    #[test]
    fn draft_rule_rejects_an_empty_any_of() {
        let nowhere = draft_rule(
            "prod",
            "CPU hot",
            MetricKind::CpuUtilization,
            Comparator::GreaterThan,
            0.9,
            Duration::seconds(120),
            HostSelector::AnyOf(vec![]),
        );
        assert!(matches!(nowhere, Err(ClientError::InvalidRule(_))));
    }

    #[test]
    fn draft_rule_rejects_a_heartbeat_rule() {
        let heartbeat = draft_rule(
            "prod",
            "Silent",
            MetricKind::AgentHeartbeat,
            Comparator::LessThan,
            1.0,
            Duration::seconds(120),
            HostSelector::All,
        );
        assert!(
            matches!(heartbeat, Err(ClientError::InvalidRule(_))),
            "a silent host is liveness's business, never an alert's"
        );
    }

    #[test]
    fn draft_rule_rejects_an_environment_containing_colon_colon() {
        let separated = draft_rule(
            "pr::od",
            "CPU hot",
            MetricKind::CpuUtilization,
            Comparator::GreaterThan,
            0.9,
            Duration::seconds(120),
            HostSelector::All,
        );
        assert!(
            matches!(separated, Err(ClientError::InvalidUrn(_))),
            "the environment is a URN segment, and a segment carrying the separator is unparseable"
        );

        let empty = draft_rule(
            "",
            "CPU hot",
            MetricKind::CpuUtilization,
            Comparator::GreaterThan,
            0.9,
            Duration::seconds(120),
            HostSelector::All,
        );
        assert!(matches!(empty, Err(ClientError::InvalidUrn(_))));
    }

    #[test]
    fn delete_rule_on_an_unknown_id_is_rule_not_found() {
        let repo = InMemoryRuleRepository::new();
        let id =
            parse_rule_id("pessimal::prod::alerts::rule::0199c0f0-0000-7000-8000-000000000001")
                .expect("valid URN");

        let missing = block_on(delete_rule(&repo, &id));

        assert!(matches!(missing, Err(ClientError::RuleNotFound(_))));
    }

    #[test]
    fn rule_fingerprint_changes_with_every_judged_field() {
        let rule = drafted();
        let baseline = rule_fingerprint(&rule);

        // Two rules with identical fields and different ids fingerprint the same: the id is the
        // key the evaluation lives under, not part of what the rule asks.
        assert_eq!(rule_fingerprint(&drafted()), baseline);

        let mut metric = rule.clone();
        metric.metric = MetricKind::MemoryUtilization;
        let mut comparator = rule.clone();
        comparator.comparator = Comparator::LessThan;
        let mut threshold = rule.clone();
        threshold.threshold = 0.95;
        let mut selector = rule.clone();
        selector.selector = HostSelector::Host(HostId::new("web-1"));
        // `for_duration` is private with no setter, so an edited dwell is a fresh draft today.
        // The identical-twin assertion above is what makes the difference attributable to the
        // dwell rather than to the new id.
        let dwell = draft_rule(
            "prod",
            "CPU hot",
            MetricKind::CpuUtilization,
            Comparator::GreaterThan,
            0.9,
            Duration::seconds(300),
            HostSelector::All,
        )
        .expect("valid rule");

        for (field, edited) in [
            ("metric", &metric),
            ("comparator", &comparator),
            ("threshold", &threshold),
            ("selector", &selector),
            ("for_duration", &dwell),
        ] {
            assert_ne!(
                rule_fingerprint(edited),
                baseline,
                "editing {field} must drop the accumulated dwell"
            );
        }

        let mut renamed = rule.clone();
        renamed.name = "CPU very hot".to_owned();
        assert_eq!(
            rule_fingerprint(&renamed),
            baseline,
            "a rename must not resolve a firing alert and re-fire it a dwell later"
        );
        assert_eq!(
            rule_fingerprint(&rule.clone().disabled()),
            baseline,
            "disabling drops the evaluation outright; the fingerprint has no work to do here"
        );

        let mut ambiguous = rule.clone();
        ambiguous.selector = HostSelector::AnyOf(vec![HostId::new("ab"), HostId::new("c")]);
        let mut split = rule.clone();
        split.selector = HostSelector::AnyOf(vec![HostId::new("a"), HostId::new("bc")]);
        assert_ne!(
            rule_fingerprint(&ambiguous),
            rule_fingerprint(&split),
            "host ids are length-prefixed so a re-split selector cannot keep its dwell"
        );
    }
}
