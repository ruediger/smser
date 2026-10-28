//! Account balance tracking from provider SMS.
//!
//! Many prepaid providers report the account balance only over SMS. Running out
//! of credit kills the SMS alerting path *silently* -- the service is up, the
//! modem is fine, the webhook is accepted, and nothing arrives. That is the same
//! failure shape as a PIN-locked SIM, so balance is worth monitoring for the
//! same reason: it protects the channel everything else depends on.
//!
//! Nothing here knows about any particular provider. The operator supplies the
//! sender to trust, a regex with one capture group, and optionally a request to
//! send on a schedule.
//!
//! # Two lessons from real inbox data
//!
//! Both of these were found by testing candidate patterns against a real inbox,
//! not by reasoning, and both are encoded as tests below.
//!
//! **Filter by sender before extracting.** An inbox accumulates messages from
//! old providers. A ten-month-old message reading "Your total outstanding
//! balance is 0 GBP" will satisfy a content-only matcher and report a zero
//! balance for a SIM that is no longer in use.
//!
//! **Anchor the pattern on the currency, not the word "balance".** In
//! `Your latest balance (last usage - 28-09-2026 01:10:07) was £10.00`, the
//! first number after "balance" is the *day of the month*. A pattern like
//! `balance[^0-9]*([0-9.]+)` yields 28.

use std::time::Duration;

use regex::Regex;
use tracing::{debug, info, warn};

use crate::types::SmsMessage;

/// How the operator configures balance tracking.
#[derive(Debug, Clone)]
pub struct BalanceConfig {
    /// Only messages from this sender are considered. Substring match, so both
    /// `07950014111` and `+447950014111` can be covered by the national part --
    /// providers commonly reply from the international form of the number you
    /// texted.
    pub from: String,
    /// Must contain exactly one capture group, holding the numeric value.
    pub pattern: Regex,
    /// Value of the `provider` metric label.
    pub provider: String,
    /// Value of the `currency` metric label. Not parsed from the message; the
    /// operator states it, because a symbol is not reliably a currency.
    pub currency: String,
    /// Where to send a request. `None` means passive: parse whatever arrives
    /// and never send anything, which costs nothing.
    pub request_to: Option<String>,
    /// Body of the request, e.g. `BALANCE`.
    pub request_text: String,
    /// How often to send a request. Ignored when `request_to` is `None`.
    pub interval: Duration,
}

impl BalanceConfig {
    pub fn is_active(&self) -> bool {
        self.request_to.is_some()
    }
}

/// The most recent successfully parsed balance, for the status page.
#[derive(Debug, Clone)]
pub struct BalanceStatus {
    pub provider: String,
    pub currency: String,
    pub value: Option<f64>,
    /// Unix seconds when the value was parsed; `None` if never.
    pub updated: Option<f64>,
    pub active: bool,
    pub parse_failures: u64,
}

/// Extract the value from one message body.
///
/// Returns `None` when the pattern does not match or the capture is not a
/// number. A non-match is deliberately not zero: reporting 0 for an
/// unparseable message is how a false low-balance alert happens.
pub fn parse_value(pattern: &Regex, content: &str) -> Option<f64> {
    let caps = pattern.captures(content)?;
    caps.get(1)?.as_str().trim().parse::<f64>().ok()
}

/// A balance read out of one provider message.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedBalance {
    pub value: f64,
    /// The modem's `Date` field for the message this came from, verbatim.
    ///
    /// Used to tell a *new* reply from the same reply seen again. The inbox is
    /// re-parsed on every poll, so without this there is no way to distinguish
    /// "the provider answered again" from "the old message is still sitting
    /// there" -- and that distinction is the whole basis of staleness
    /// detection.
    pub message_date: String,
}

/// Convert the modem's `Date` field to a Unix timestamp.
///
/// # The timezone is deliberately ignored
///
/// The modem reports `YYYY-MM-DD HH:MM:SS` in **its own local time** with no
/// offset, and this treats it as UTC. On a host in Europe/London that is a skew
/// of up to one hour, and the sign flips with daylight saving.
///
/// That is an accepted trade, not an oversight:
///
/// * The only consumer is `SmserBalanceStale`, which asks whether a reply has
///   arrived in the last **nine days**. One hour is 0.5% of that threshold.
/// * Resolving it properly needs the host's UTC offset, and
///   `UtcOffset::current_local_offset()` deliberately fails in a multi-threaded
///   process on Unix -- which smser is. Working around that means either
///   reading the offset before the runtime starts or hardcoding a zone, both of
///   which are more fragile than the error they remove.
///
/// So a balance can read as up to an hour older or newer than it truly is.
/// Anything comparing this against wall-clock time to the minute will be wrong;
/// nothing does.
///
/// Returns `None` when the field is not in the expected shape, so the caller
/// can fall back rather than publishing a nonsense timestamp.
pub fn message_date_to_unix(date: &str) -> Option<f64> {
    let (d, t) = date.trim().split_once(' ')?;
    let mut dp = d.split('-');
    let (y, mo, da) = (dp.next()?, dp.next()?, dp.next()?);
    let mut tp = t.split(':');
    let (h, mi, se) = (tp.next()?, tp.next()?, tp.next()?);

    let date = time::Date::from_calendar_date(
        y.parse().ok()?,
        time::Month::try_from(mo.parse::<u8>().ok()?).ok()?,
        da.parse().ok()?,
    )
    .ok()?;
    let time_of_day =
        time::Time::from_hms(h.parse().ok()?, mi.parse().ok()?, se.parse().ok()?).ok()?;

    Some(date.with_time(time_of_day).assume_utc().unix_timestamp() as f64)
}

/// Pick the newest message from the configured sender whose body parses.
///
/// Messages are ordered by the modem's `Date` field, which is
/// `YYYY-MM-DD HH:MM:SS` and therefore sorts correctly as a string.
pub fn latest_balance(config: &BalanceConfig, messages: &[SmsMessage]) -> Option<ParsedBalance> {
    let mut candidates: Vec<&SmsMessage> = messages
        .iter()
        .filter(|m| m.phone.contains(&config.from))
        .collect();
    candidates.sort_by(|a, b| b.date.cmp(&a.date));

    for m in candidates {
        if let Some(value) = parse_value(&config.pattern, &m.content) {
            return Some(ParsedBalance {
                value,
                message_date: m.date.clone(),
            });
        }
        debug!(
            "Message from {} did not match the balance pattern",
            config.from
        );
    }
    None
}

fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Apply a newly-arrived balance reply to the metrics.
///
/// Call this only when the message is one not seen before, identified by its
/// date. The timestamp gauge means "when the provider last answered", which is
/// what staleness detection needs -- so it must advance on every new reply
/// whether or not the value moved, and must NOT advance when the same old
/// message is re-parsed on the next poll.
///
/// Getting this wrong is not hypothetical: gating this on the *value* having
/// changed made `SmserBalanceStale` fire after nine days of a stable balance,
/// which is the normal state for a SIM that sends a few texts a month.
pub fn record_balance(config: &BalanceConfig, balance: &ParsedBalance) {
    metrics::gauge!(
        "smser_account_balance",
        "provider" => config.provider.clone(),
        "currency" => config.currency.clone()
    )
    .set(balance.value);
    // The reply's own date, not when we read it -- so a restart cannot reset
    // the staleness clock and make an old figure look fresh. Falls back to now
    // only if the modem sends a date we cannot parse, which would otherwise
    // publish a nonsense timestamp.
    let stamp = message_date_to_unix(&balance.message_date).unwrap_or_else(|| {
        warn!(
            "Could not parse the balance reply date {:?}; falling back to the current time",
            balance.message_date
        );
        now_unix()
    });
    metrics::gauge!("smser_account_balance_timestamp_seconds").set(stamp);
    metrics::counter!("smser_balance_replies_parsed_total").increment(1);
    info!(
        "Account balance for {}: {} {} (reply dated {})",
        config.provider, balance.value, config.currency, balance.message_date
    );
}

/// Note that a message from the balance sender arrived but could not be parsed.
///
/// The gauge is deliberately left untouched -- a stale-but-real value is more
/// useful than a fabricated one, and the staleness is visible via the timestamp.
pub fn record_parse_failure(config: &BalanceConfig, content: &str) {
    metrics::counter!("smser_balance_parse_failures_total").increment(1);
    warn!(
        "Message from the {} balance sender did not match the configured pattern; \
         balance left unchanged. First 60 chars: {}",
        config.provider,
        content.chars().take(60).collect::<String>()
    );
}

pub fn record_request_sent() {
    metrics::counter!("smser_balance_requests_sent_total").increment(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Priority, SmsStat, SmsType};

    /// The pattern a 1pmobile user would configure. Anchored on the currency
    /// symbol, which is what makes it correct -- see the date-trap test.
    fn pound_pattern() -> Regex {
        Regex::new(r"£([0-9]+\.[0-9]{2})").unwrap()
    }

    fn config() -> BalanceConfig {
        BalanceConfig {
            from: "7950014111".to_string(),
            pattern: pound_pattern(),
            provider: "testprovider".to_string(),
            currency: "GBP".to_string(),
            request_to: None,
            request_text: "BALANCE".to_string(),
            interval: Duration::from_secs(604_800),
        }
    }

    fn msg(index: i32, phone: &str, date: &str, content: &str) -> SmsMessage {
        SmsMessage {
            smstat: SmsStat::Unread,
            index,
            phone: phone.to_string(),
            content: content.to_string(),
            date: date.to_string(),
            sca: String::new(),
            save_type: 0,
            priority: Priority::Normal,
            sms_type: SmsType::Single,
        }
    }

    /// Verbatim from a real reply.
    const REAL_REPLY: &str = "Your latest balance (last usage - 28-09-2026 01:10:07) \
was £10.00. You have 1024MB of your data Boost available.";

    #[test]
    fn test_message_date_to_unix() {
        // Verified against `date -u -d "2026-10-20 23:31:57" +%s`.
        assert_eq!(
            message_date_to_unix("2026-10-20 23:31:57"),
            Some(1792539117.0)
        );
        assert_eq!(message_date_to_unix("1970-01-01 00:00:00"), Some(0.0));
        // Leap day, since the parser builds a real calendar date.
        assert_eq!(
            message_date_to_unix("2024-02-29 12:00:00"),
            Some(1709208000.0)
        );
        // Surrounding whitespace is tolerated.
        assert_eq!(
            message_date_to_unix("  2026-10-20 23:31:57 "),
            Some(1792539117.0)
        );
    }

    #[test]
    fn test_message_date_rejects_nonsense() {
        // Each must yield None so the caller falls back rather than publishing
        // a garbage timestamp.
        for bad in [
            "",
            "not a date",
            "2026-10-20",          // no time
            "2026-13-01 00:00:00", // month 13
            "2026-02-30 00:00:00", // not a real day
            "2026-10-20 25:00:00", // hour 25
            "2026-10-20 23:61:00", // minute 61
            "2026/10/20 23:31:57", // wrong separators
        ] {
            assert!(
                message_date_to_unix(bad).is_none(),
                "should have rejected {:?}",
                bad
            );
        }
    }

    #[test]
    fn test_message_date_ordering_matches_string_ordering() {
        // latest_balance sorts reply dates as strings; that must agree with
        // chronological order or it could pick an older reply.
        let dates = [
            "2026-09-30 23:59:59",
            "2026-10-01 00:00:00",
            "2026-10-06 01:05:57",
            "2026-10-20 23:31:57",
        ];
        let stamps: Vec<f64> = dates
            .iter()
            .map(|d| message_date_to_unix(d).unwrap())
            .collect();
        let mut sorted = stamps.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(
            stamps, sorted,
            "string order diverged from chronological order"
        );
    }

    #[test]
    fn test_parsed_balance_carries_the_message_date() {
        // The date is what distinguishes a new reply from the same reply seen
        // again, so it must survive parsing.
        let messages = vec![msg(1, "+447950014111", "2026-10-20 23:31:57", REAL_REPLY)];
        let got = latest_balance(&config(), &messages).expect("should parse");
        assert_eq!(got.value, 10.00);
        assert_eq!(got.message_date, "2026-10-20 23:31:57");
    }

    #[test]
    fn test_same_value_from_a_newer_reply_is_a_distinct_observation() {
        // The regression this guards against: a provider that answers weekly
        // with an unchanged balance. Keying freshness off the value made
        // SmserBalanceStale fire after nine days of a perfectly healthy SIM.
        // Keying it off the message date distinguishes the two replies.
        let week1 = vec![msg(1, "+447950014111", "2026-10-13 23:31:57", REAL_REPLY)];
        let week2 = vec![msg(2, "+447950014111", "2026-10-20 23:31:57", REAL_REPLY)];

        let a = latest_balance(&config(), &week1).unwrap();
        let b = latest_balance(&config(), &week2).unwrap();

        assert_eq!(a.value, b.value, "the balance genuinely did not change");
        assert_ne!(
            a.message_date, b.message_date,
            "but they are different replies, and freshness must follow the reply"
        );
    }

    #[test]
    fn test_reparsing_the_same_inbox_yields_an_identical_observation() {
        // The other half: polling again with nothing new must look identical,
        // so the caller can tell there is no new reply and leave the timestamp
        // alone.
        let messages = vec![msg(1, "+447950014111", "2026-10-20 23:31:57", REAL_REPLY)];
        assert_eq!(
            latest_balance(&config(), &messages),
            latest_balance(&config(), &messages)
        );
    }

    #[test]
    fn test_parses_the_real_reply() {
        assert_eq!(parse_value(&pound_pattern(), REAL_REPLY), Some(10.00));
    }

    #[test]
    fn test_the_date_trap() {
        // The first number after the word "balance" is the day of the month, so
        // a pattern anchored on that word extracts 28 rather than 10.00. This
        // test exists to document why the currency anchor is required.
        let naive = Regex::new(r"balance[^0-9]*([0-9]+(?:\.[0-9]{2})?)").unwrap();
        assert_eq!(parse_value(&naive, REAL_REPLY), Some(28.0));
        assert_eq!(parse_value(&pound_pattern(), REAL_REPLY), Some(10.00));
    }

    #[test]
    fn test_old_provider_message_is_excluded_by_sender() {
        // Verbatim from a real inbox: a message from a provider no longer in
        // use. Content-only matching would report a zero balance from it.
        let stale = "Your total outstanding balance is 0 GBP 30.04 GB data, \
Unlimited UK minutes, Unlimited UK text, 100 Intl minutes";
        let messages = vec![
            msg(1, "38885", "2025-12-15 00:11:37", stale),
            msg(2, "+447950014111", "2026-09-28 23:49:25", REAL_REPLY),
        ];
        assert_eq!(
            latest_balance(&config(), &messages).map(|b| b.value),
            Some(10.00)
        );

        // And on its own it is ignored entirely rather than read as zero.
        let only_stale = vec![msg(1, "38885", "2025-12-15 00:11:37", stale)];
        assert!(latest_balance(&config(), &only_stale).is_none());
    }

    #[test]
    fn test_picks_the_newest_reply() {
        let messages = vec![
            msg(
                1,
                "+447950014111",
                "2026-09-01 10:00:00",
                "Your latest balance was £3.21.",
            ),
            msg(
                2,
                "+447950014111",
                "2026-09-28 23:49:25",
                "Your latest balance was £10.00.",
            ),
            msg(
                3,
                "+447950014111",
                "2026-09-14 12:00:00",
                "Your latest balance was £7.77.",
            ),
        ];
        assert_eq!(
            latest_balance(&config(), &messages).map(|b| b.value),
            Some(10.00)
        );
    }

    #[test]
    fn test_national_form_matches_international_sender() {
        // Requests go to 07950014111; replies arrive from +447950014111.
        let messages = vec![msg(1, "+447950014111", "2026-09-28 23:49:25", REAL_REPLY)];
        assert_eq!(
            latest_balance(&config(), &messages).map(|b| b.value),
            Some(10.00)
        );
    }

    #[test]
    fn test_unparseable_reply_yields_none_not_zero() {
        let messages = vec![msg(
            1,
            "+447950014111",
            "2026-09-28 23:49:25",
            "Your account is being upgraded, balance unavailable.",
        )];
        assert!(latest_balance(&config(), &messages).is_none());
    }

    #[test]
    fn test_falls_back_to_an_older_parseable_reply() {
        // A newer unparseable message must not mask a usable older one.
        let messages = vec![
            msg(
                1,
                "+447950014111",
                "2026-09-28 23:49:25",
                "Service message: no balance included.",
            ),
            msg(
                2,
                "+447950014111",
                "2026-09-20 10:00:00",
                "Your latest balance was £4.50.",
            ),
        ];
        assert_eq!(
            latest_balance(&config(), &messages).map(|b| b.value),
            Some(4.50)
        );
    }

    #[test]
    fn test_pattern_shapes_other_providers_might_need() {
        for (pat, text, want) in [
            (
                r"([0-9]+\.[0-9]{2})\s*GBP",
                "Balance: 12.34 GBP remaining",
                12.34,
            ),
            (r"([0-9]+)p\b", "You have 483p credit", 483.0),
            (r"\$([0-9]+\.[0-9]{2})", "Your balance is $7.05", 7.05),
        ] {
            let re = Regex::new(pat).unwrap();
            assert_eq!(parse_value(&re, text), Some(want), "pattern {}", pat);
        }
    }

    #[test]
    fn test_is_active() {
        assert!(!config().is_active());
        let active = BalanceConfig {
            request_to: Some("07950014111".to_string()),
            ..config()
        };
        assert!(active.is_active());
    }
}
