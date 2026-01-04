use metrics::{Unit, describe_counter, describe_gauge, gauge};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

static PROMETHEUS_HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Per-client rate limit configuration
#[derive(Clone, Debug, PartialEq)]
pub struct ClientLimit {
    pub name: String,
    pub hourly_limit: u32,
    pub daily_limit: u32,
}

impl ClientLimit {
    /// Parse a client limit from "name:hourly:daily" format
    pub fn parse(s: &str) -> Result<Self, String> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 3 {
            return Err(format!(
                "Invalid client limit format '{}'. Expected 'name:hourly:daily'",
                s
            ));
        }
        let name = parts[0].to_string();
        if name.is_empty() {
            return Err("Client name cannot be empty".to_string());
        }
        let hourly_limit = parts[1]
            .parse()
            .map_err(|_| format!("Invalid hourly limit '{}'", parts[1]))?;
        let daily_limit = parts[2]
            .parse()
            .map_err(|_| format!("Invalid daily limit '{}'", parts[2]))?;
        Ok(Self {
            name,
            hourly_limit,
            daily_limit,
        })
    }
}

#[derive(Debug, Serialize)]
pub struct RateLimitStatus {
    pub hourly_usage: u32,
    pub hourly_limit: u32,
    pub daily_usage: u32,
    pub daily_limit: u32,
}

#[derive(Clone, Debug)]
pub struct RateLimiter {
    hourly_limit: u32,
    daily_limit: u32,
    client_limits: HashMap<String, (u32, u32)>, // name -> (hourly, daily)
    state: Arc<Mutex<RateLimitState>>,
}

#[derive(Debug)]
struct RateLimitState {
    hourly_count: u32,
    daily_count: u32,
    last_reset_hour: Instant,
    last_reset_day: Instant,
    // Per-client state: name -> (hourly_count, daily_count, last_reset_hour, last_reset_day)
    client_state: HashMap<String, ClientRateLimitState>,
}

#[derive(Debug)]
struct ClientRateLimitState {
    hourly_count: u32,
    daily_count: u32,
    last_reset_hour: Instant,
    last_reset_day: Instant,
}

impl ClientRateLimitState {
    fn new() -> Self {
        Self {
            hourly_count: 0,
            daily_count: 0,
            last_reset_hour: Instant::now(),
            last_reset_day: Instant::now(),
        }
    }

    fn update(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_reset_hour) >= Duration::from_secs(3600) {
            self.hourly_count = 0;
            self.last_reset_hour = now;
        }
        if now.duration_since(self.last_reset_day) >= Duration::from_secs(86400) {
            self.daily_count = 0;
            self.last_reset_day = now;
        }
    }
}

impl RateLimitState {
    fn update(&mut self) {
        let now = Instant::now();
        // Check if hour has passed
        if now.duration_since(self.last_reset_hour) >= Duration::from_secs(3600) {
            self.hourly_count = 0;
            self.last_reset_hour = now;
        }

        // Check if day has passed
        if now.duration_since(self.last_reset_day) >= Duration::from_secs(86400) {
            self.daily_count = 0;
            self.last_reset_day = now;
        }
    }
}

impl RateLimiter {
    pub fn new(hourly_limit: u32, daily_limit: u32, client_limits: Vec<ClientLimit>) -> Self {
        let client_limits_map: HashMap<String, (u32, u32)> = client_limits
            .into_iter()
            .map(|cl| (cl.name, (cl.hourly_limit, cl.daily_limit)))
            .collect();
        Self {
            hourly_limit,
            daily_limit,
            client_limits: client_limits_map,
            state: Arc::new(Mutex::new(RateLimitState {
                hourly_count: 0,
                daily_count: 0,
                last_reset_hour: Instant::now(),
                last_reset_day: Instant::now(),
                client_state: HashMap::new(),
            })),
        }
    }

    pub fn check_and_increment(&self, client: Option<&str>) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        state.update();

        // Check global limits first
        if state.hourly_count >= self.hourly_limit {
            return Err(format!("Hourly limit of {} reached", self.hourly_limit));
        }

        if state.daily_count >= self.daily_limit {
            return Err(format!("Daily limit of {} reached", self.daily_limit));
        }

        // Check per-client limits if client is specified and configured
        if let Some(client_name) = client
            && let Some(&(client_hourly, client_daily)) = self.client_limits.get(client_name)
        {
            // Get or create client state
            let client_state = state
                .client_state
                .entry(client_name.to_string())
                .or_insert_with(ClientRateLimitState::new);
            client_state.update();

            if client_state.hourly_count >= client_hourly {
                return Err(format!(
                    "Client '{}' hourly limit of {} reached",
                    client_name, client_hourly
                ));
            }

            if client_state.daily_count >= client_daily {
                return Err(format!(
                    "Client '{}' daily limit of {} reached",
                    client_name, client_daily
                ));
            }

            // Increment client counters
            client_state.hourly_count += 1;
            client_state.daily_count += 1;

            // Update client metrics
            gauge!("smser_client_hourly_usage", "client" => client_name.to_string())
                .set(client_state.hourly_count as f64);
            gauge!("smser_client_daily_usage", "client" => client_name.to_string())
                .set(client_state.daily_count as f64);
        }
        // If client name provided but not configured, just use global limits

        // Increment global counters
        state.hourly_count += 1;
        state.daily_count += 1;

        // Update global metrics
        gauge!("smser_hourly_usage").set(state.hourly_count as f64);
        gauge!("smser_daily_usage").set(state.daily_count as f64);

        Ok(())
    }

    pub fn get_status(&self) -> RateLimitStatus {
        let mut state = self.state.lock().unwrap();
        state.update();

        RateLimitStatus {
            hourly_usage: state.hourly_count,
            hourly_limit: self.hourly_limit,
            daily_usage: state.daily_count,
            daily_limit: self.daily_limit,
        }
    }
}

pub fn setup_metrics() -> PrometheusHandle {
    PROMETHEUS_HANDLE
        .get_or_init(|| {
            let builder = PrometheusBuilder::new();
            let handle = builder
                .install_recorder()
                .expect("failed to install Prometheus recorder");

            describe_counter!(
                "smser_sms_sent_total",
                Unit::Count,
                "Total number of SMS sent"
            );
            describe_counter!(
                "smser_http_requests_total",
                Unit::Count,
                "Total number of HTTP requests"
            );
            describe_counter!(
                "smser_sms_country_total",
                Unit::Count,
                "Total number of SMS sent by country code"
            );
            describe_gauge!("smser_hourly_limit", Unit::Count, "Hourly SMS limit");
            describe_gauge!("smser_daily_limit", Unit::Count, "Daily SMS limit");
            describe_gauge!(
                "smser_hourly_usage",
                Unit::Count,
                "Current hourly SMS usage"
            );
            describe_gauge!("smser_daily_usage", Unit::Count, "Current daily SMS usage");
            describe_gauge!(
                "smser_client_hourly_usage",
                Unit::Count,
                "Current hourly SMS usage per client"
            );
            describe_gauge!(
                "smser_client_daily_usage",
                Unit::Count,
                "Current daily SMS usage per client"
            );
            describe_gauge!(
                "smser_client_hourly_limit",
                Unit::Count,
                "Configured hourly SMS limit per client"
            );
            describe_gauge!(
                "smser_client_daily_limit",
                Unit::Count,
                "Configured daily SMS limit per client"
            );
            describe_gauge!(
                "smser_sms_stored",
                Unit::Count,
                "Number of SMS messages stored on the SIM"
            );
            describe_gauge!(
                "smser_start_time_seconds",
                Unit::Seconds,
                "Start time of the server in Unix seconds"
            );
            describe_gauge!(
                "smser_version_info",
                Unit::Count,
                "Version information of the server"
            );

            handle
        })
        .clone()
}

pub fn update_limits_metrics(hourly: u32, daily: u32) {
    gauge!("smser_hourly_limit").set(hourly as f64);
    gauge!("smser_daily_limit").set(daily as f64);
}

pub fn update_client_limits_metrics(client_limits: &[ClientLimit]) {
    for cl in client_limits {
        gauge!("smser_client_hourly_limit", "client" => cl.name.clone())
            .set(cl.hourly_limit as f64);
        gauge!("smser_client_daily_limit", "client" => cl.name.clone())
            .set(cl.daily_limit as f64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limiter_hourly_limit() {
        let limiter = RateLimiter::new(2, 10, vec![]);
        assert!(limiter.check_and_increment(None).is_ok());
        assert!(limiter.check_and_increment(None).is_ok());
        let result = limiter.check_and_increment(None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Hourly limit of 2 reached"));
    }

    #[test]
    fn test_rate_limiter_daily_limit() {
        let limiter = RateLimiter::new(10, 2, vec![]);
        assert!(limiter.check_and_increment(None).is_ok());
        assert!(limiter.check_and_increment(None).is_ok());
        let result = limiter.check_and_increment(None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Daily limit of 2 reached"));
    }

    #[test]
    fn test_rate_limiter_client_limit() {
        let client_limits = vec![ClientLimit {
            name: "test_client".to_string(),
            hourly_limit: 2,
            daily_limit: 10,
        }];
        let limiter = RateLimiter::new(100, 1000, client_limits);

        // Client-specific limits
        assert!(limiter.check_and_increment(Some("test_client")).is_ok());
        assert!(limiter.check_and_increment(Some("test_client")).is_ok());
        let result = limiter.check_and_increment(Some("test_client"));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("Client 'test_client' hourly limit of 2 reached"));

        // Unknown client uses global limits
        assert!(limiter.check_and_increment(Some("unknown_client")).is_ok());
    }

    #[test]
    fn test_rate_limiter_client_counts_against_global() {
        let client_limits = vec![ClientLimit {
            name: "test_client".to_string(),
            hourly_limit: 10,
            daily_limit: 100,
        }];
        let limiter = RateLimiter::new(2, 10, client_limits);

        // Client requests count against global limit
        assert!(limiter.check_and_increment(Some("test_client")).is_ok());
        assert!(limiter.check_and_increment(Some("test_client")).is_ok());
        // Global limit reached
        let result = limiter.check_and_increment(Some("test_client"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Hourly limit of 2 reached"));
    }

    #[test]
    fn test_client_limit_parse() {
        let cl = ClientLimit::parse("myapp:5:20").unwrap();
        assert_eq!(cl.name, "myapp");
        assert_eq!(cl.hourly_limit, 5);
        assert_eq!(cl.daily_limit, 20);

        assert!(ClientLimit::parse("invalid").is_err());
        assert!(ClientLimit::parse(":5:20").is_err());
        assert!(ClientLimit::parse("name:abc:20").is_err());
    }

    #[test]
    fn test_rate_limiter_reset_logic_simulated() {
        // Since we can't easily mock Instant::now() without extra dependencies,
        // we can at least test that if we manually manipulate the state (if it were accessible)
        // but for now, the basic enforcement tests above cover the core logic.
    }
}
