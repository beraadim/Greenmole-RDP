//! Small, expiring failure budgets. Rejected retries never prolong a cooldown;
//! a successful authentication clears that source, including a shared gateway.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::time::Duration;

use tokio::time::Instant;

const MAX_SOURCES: usize = 256;
const SOURCE_EXPIRY: Duration = Duration::from_secs(60);
const FREE_FAILURES: u8 = 5;
const GLOBAL_WINDOW: Duration = Duration::from_secs(10);
const GLOBAL_FAILURES: usize = 20;
const GLOBAL_COOLDOWN: Duration = Duration::from_millis(250);

struct SourceFailures {
    count: u8,
    last_failure: Instant,
    retry_at: Instant,
}

pub(super) struct AdmissionLimiter {
    sources: HashMap<IpAddr, SourceFailures>,
    recent_failures: VecDeque<Instant>,
    global_retry_at: Instant,
}

impl Default for AdmissionLimiter {
    fn default() -> Self {
        Self {
            sources: HashMap::new(),
            recent_failures: VecDeque::new(),
            global_retry_at: Instant::now(),
        }
    }
}

fn source_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(ip)),
        ip => ip,
    }
}

impl AdmissionLimiter {
    pub(super) fn global_retry_at(&self) -> Instant {
        self.global_retry_at
    }

    fn expire(&mut self, now: Instant) {
        self.sources
            .retain(|_, entry| now.saturating_duration_since(entry.last_failure) < SOURCE_EXPIRY);
        while self
            .recent_failures
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= GLOBAL_WINDOW)
        {
            self.recent_failures.pop_front();
        }
    }

    pub(super) fn retry_after(&mut self, ip: IpAddr, now: Instant) -> Option<Duration> {
        self.expire(now);
        self.sources
            .get(&source_key(ip))
            .and_then(|entry| entry.retry_at.checked_duration_since(now))
            .filter(|delay| !delay.is_zero())
    }

    pub(super) fn succeeded(&mut self, ip: IpAddr) {
        self.sources.remove(&source_key(ip));
    }

    pub(super) fn failed(&mut self, ip: IpAddr, now: Instant) {
        self.expire(now);
        let ip = source_key(ip);
        if !self.sources.contains_key(&ip) && self.sources.len() >= MAX_SOURCES {
            if let Some(oldest) = self
                .sources
                .iter()
                .min_by_key(|(_, entry)| entry.last_failure)
                .map(|(ip, _)| *ip)
            {
                self.sources.remove(&oldest);
            }
        }
        let entry = self.sources.entry(ip).or_insert(SourceFailures {
            count: 0,
            last_failure: now,
            retry_at: now,
        });
        entry.count = entry.count.saturating_add(1);
        entry.last_failure = now;
        let delay = if entry.count <= FREE_FAILURES {
            Duration::ZERO
        } else {
            // Keep shared Guacamole gateways usable: short bounded pauses,
            // never an account lockout or an escalating multi-minute ban.
            Duration::from_millis(250u64 << (entry.count - FREE_FAILURES - 1).min(3))
        };
        entry.retry_at = now + delay;
        if self.recent_failures.len() == GLOBAL_FAILURES {
            self.recent_failures.pop_front();
        }
        self.recent_failures.push_back(now);
        if self.recent_failures.len() == GLOBAL_FAILURES {
            self.global_retry_at = now + GLOBAL_COOLDOWN;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(n: u16) -> IpAddr {
        IpAddr::V4(std::net::Ipv4Addr::new(192, 0, (n >> 8) as u8, n as u8))
    }

    #[test]
    fn gateway_has_grace_attempts_and_retries_do_not_extend_cooldown() {
        let now = Instant::now();
        let mut limiter = AdmissionLimiter::default();
        for _ in 0..FREE_FAILURES {
            limiter.failed(source(1), now);
        }
        assert!(limiter.retry_after(source(1), now).is_none());
        limiter.failed(source(1), now);
        assert_eq!(
            limiter.retry_after(source(1), now),
            Some(Duration::from_millis(250))
        );
        for _ in 0..100 {
            assert!(limiter
                .retry_after(source(1), now + Duration::from_millis(100))
                .is_some());
        }
        assert!(limiter
            .retry_after(source(1), now + Duration::from_millis(250))
            .is_none());
        limiter.succeeded(source(1));
        limiter.failed(source(1), now);
        assert!(limiter.retry_after(source(1), now).is_none());
    }

    #[test]
    fn sources_expire_and_tracking_and_global_window_stay_bounded() {
        let now = Instant::now();
        let mut limiter = AdmissionLimiter::default();
        for n in 0..1000 {
            limiter.failed(source(n), now);
        }
        assert_eq!(limiter.sources.len(), MAX_SOURCES);
        assert_eq!(limiter.recent_failures.len(), GLOBAL_FAILURES);
        assert_eq!(limiter.global_retry_at(), now + GLOBAL_COOLDOWN);
        assert!(limiter
            .retry_after(source(999), now + SOURCE_EXPIRY)
            .is_none());
        assert!(limiter.sources.is_empty());
        assert!(limiter.recent_failures.is_empty());
    }

    #[test]
    fn long_failure_streak_is_capped_and_mapped_ipv4_uses_same_budget() {
        let now = Instant::now();
        let mut limiter = AdmissionLimiter::default();
        for _ in 0..300 {
            limiter.failed(source(1), now);
        }
        assert_eq!(
            limiter.retry_after(source(1), now),
            Some(Duration::from_secs(2))
        );
        let IpAddr::V4(ip) = source(1) else {
            unreachable!()
        };
        assert_eq!(
            limiter.retry_after(IpAddr::V6(ip.to_ipv6_mapped()), now),
            Some(Duration::from_secs(2))
        );
        assert!(limiter.retry_after(source(2), now).is_none());
    }
}
