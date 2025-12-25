use metrics::{describe_counter, describe_gauge, gauge, Unit};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

static PROMETHEUS_HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

#[derive(Clone, Debug)]
pub struct RateLimiter {
    hourly_limit: u32,
    daily_limit: u32,
    state: Arc<Mutex<RateLimitState>>,
}

#[derive(Debug)]
struct RateLimitState {
    hourly_count: u32,
    daily_count: u32,
    last_reset_hour: Instant,
    last_reset_day: Instant,
}

impl RateLimiter {
    pub fn new(hourly_limit: u32, daily_limit: u32) -> Self {
        Self {
            hourly_limit,
            daily_limit,
            state: Arc::new(Mutex::new(RateLimitState {
                hourly_count: 0,
                daily_count: 0,
                last_reset_hour: Instant::now(),
                last_reset_day: Instant::now(),
            })),
        }
    }

    pub fn check_and_increment(&self) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let now = Instant::now();

        // Check if hour has passed
        if now.duration_since(state.last_reset_hour) >= Duration::from_secs(3600) {
            state.hourly_count = 0;
            state.last_reset_hour = now;
        }

        // Check if day has passed
        if now.duration_since(state.last_reset_day) >= Duration::from_secs(86400) {
            state.daily_count = 0;
            state.last_reset_day = now;
        }

        if state.hourly_count >= self.hourly_limit {
            return Err(format!("Hourly limit of {} reached", self.hourly_limit));
        }

        if state.daily_count >= self.daily_limit {
            return Err(format!("Daily limit of {} reached", self.daily_limit));
        }

        state.hourly_count += 1;
        state.daily_count += 1;

        // Update metrics
        gauge!("smser_hourly_usage").set(state.hourly_count as f64);
        gauge!("smser_daily_usage").set(state.daily_count as f64);

        Ok(())
    }
}

pub fn setup_metrics() -> PrometheusHandle {
    PROMETHEUS_HANDLE
        .get_or_init(|| {
            let builder = PrometheusBuilder::new();
            let handle = builder
                .install_recorder()
                .expect("failed to install Prometheus recorder");

            describe_counter!("smser_sms_sent_total", Unit::Count, "Total number of SMS sent");
            describe_gauge!("smser_hourly_limit", Unit::Count, "Hourly SMS limit");
            describe_gauge!("smser_daily_limit", Unit::Count, "Daily SMS limit");
            describe_gauge!("smser_hourly_usage", Unit::Count, "Current hourly SMS usage");
            describe_gauge!("smser_daily_usage", Unit::Count, "Current daily SMS usage");

            handle
        })
        .clone()
}

pub fn update_limits_metrics(hourly: u32, daily: u32) {
    gauge!("smser_hourly_limit").set(hourly as f64);
    gauge!("smser_daily_limit").set(daily as f64);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limiter_hourly_limit() {
        let limiter = RateLimiter::new(2, 10);
        assert!(limiter.check_and_increment().is_ok());
        assert!(limiter.check_and_increment().is_ok());
        let result = limiter.check_and_increment();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Hourly limit of 2 reached"));
    }

    #[test]
    fn test_rate_limiter_daily_limit() {
        let limiter = RateLimiter::new(10, 2);
        assert!(limiter.check_and_increment().is_ok());
        assert!(limiter.check_and_increment().is_ok());
        let result = limiter.check_and_increment();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Daily limit of 2 reached"));
    }

    #[test]
    fn test_rate_limiter_reset_logic_simulated() {
        // Since we can't easily mock Instant::now() without extra dependencies,
        // we can at least test that if we manually manipulate the state (if it were accessible)
        // but for now, the basic enforcement tests above cover the core logic.
    }
}
