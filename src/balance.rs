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

/// Pick the newest message from the configured sender whose body parses.
///
/// Messages are ordered by the modem's `Date` field, which is
/// `YYYY-MM-DD HH:MM:SS` and therefore sorts correctly as a string.
pub fn latest_balance(config: &BalanceConfig, messages: &[SmsMessage]) -> Option<f64> {
    let mut candidates: Vec<&SmsMessage> = messages
        .iter()
        .filter(|m| m.phone.contains(&config.from))
        .collect();
    candidates.sort_by(|a, b| b.date.cmp(&a.date));

    for m in candidates {
        if let Some(v) = parse_value(&config.pattern, &m.content) {
            return Some(v);
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

/// Apply a parsed balance to the metrics.
pub fn record_balance(config: &BalanceConfig, value: f64) {
    metrics::gauge!(
        "smser_account_balance",
        "provider" => config.provider.clone(),
        "currency" => config.currency.clone()
    )
    .set(value);
    metrics::gauge!("smser_account_balance_timestamp_seconds").set(now_unix());
    metrics::counter!("smser_balance_replies_parsed_total").increment(1);
    info!(
        "Account balance for {}: {} {}",
        config.provider, value, config.currency
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
        assert_eq!(latest_balance(&config(), &messages), Some(10.00));

        // And on its own it is ignored entirely rather than read as zero.
        let only_stale = vec![msg(1, "38885", "2025-12-15 00:11:37", stale)];
        assert_eq!(latest_balance(&config(), &only_stale), None);
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
        assert_eq!(latest_balance(&config(), &messages), Some(10.00));
    }

    #[test]
    fn test_national_form_matches_international_sender() {
        // Requests go to 07950014111; replies arrive from +447950014111.
        let messages = vec![msg(1, "+447950014111", "2026-09-28 23:49:25", REAL_REPLY)];
        assert_eq!(latest_balance(&config(), &messages), Some(10.00));
    }

    #[test]
    fn test_unparseable_reply_yields_none_not_zero() {
        let messages = vec![msg(
            1,
            "+447950014111",
            "2026-09-28 23:49:25",
            "Your account is being upgraded, balance unavailable.",
        )];
        assert_eq!(latest_balance(&config(), &messages), None);
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
        assert_eq!(latest_balance(&config(), &messages), Some(4.50));
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
