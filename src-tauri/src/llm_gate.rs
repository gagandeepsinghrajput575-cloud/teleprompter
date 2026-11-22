use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub const MIN_COOLDOWN: Duration = Duration::from_millis(1000);
const MAX_COOLDOWN: Duration = Duration::from_secs(30);
/// Spend at most 80% of the provider ITPM: headroom for clock skew and
/// other consumers sharing the same key.
const BUDGET_FRACTION: f64 = 0.80;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Hold {
    Cooldown,
    InFlight,
    Budget,
}

pub struct LlmGate {
    cooldown: Duration,
    next_allowed: Instant,
    in_flight: bool,
    itpm_budget: u32,
    spent: VecDeque<(Instant, u32)>,
}

impl LlmGate {
    pub fn new(itpm_limit: u32, now: Instant) -> Self {
        Self {
            cooldown: MIN_COOLDOWN,
            next_allowed: now,
            in_flight: false,
            itpm_budget: (itpm_limit as f64 * BUDGET_FRACTION) as u32,
            spent: VecDeque::new(),
        }
    }

    fn tokens_last_minute(&mut self, now: Instant) -> u32 {
        while let Some(&(t, _)) = self.spent.front() {
            if now.duration_since(t) >= Duration::from_secs(60) {
                self.spent.pop_front();
            } else {
                break;
            }
        }
        self.spent.iter().map(|&(_, n)| n).sum()
    }

    pub fn try_acquire(&mut self, now: Instant, est_tokens: u32) -> Result<(), Hold> {
        if self.in_flight {
            return Err(Hold::InFlight);
        }
        if now < self.next_allowed {
            return Err(Hold::Cooldown);
        }
        if self.tokens_last_minute(now) + est_tokens > self.itpm_budget {
            return Err(Hold::Budget);
        }
        self.spent.push_back((now, est_tokens));
        self.in_flight = true;
        self.next_allowed = now + self.cooldown;
        Ok(())
    }

    pub fn on_success(&mut self, now: Instant) {
        self.in_flight = false;
        // decay 25% per success back toward the floor
        self.cooldown = self.cooldown.mul_f64(0.75).max(MIN_COOLDOWN);
        self.next_allowed = self.next_allowed.max(now + self.cooldown);
    }

    /// HTTP 429: exponential backoff, honouring Retry-After when present.
    pub fn on_rate_limited(&mut self, now: Instant, retry_after: Option<Duration>) {
        self.in_flight = false;
        self.cooldown = (self.cooldown * 2)
            .max(retry_after.unwrap_or_default())
            .min(MAX_COOLDOWN);
        self.next_allowed = now + self.cooldown;
    }

    /// Network / 5xx / parse error: release the slot, keep the cooldown.
    pub fn on_failure(&mut self) {
        self.in_flight = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_gate_cooldown_budget_backoff() {
        let t0 = Instant::now();
        let mut g = LlmGate::new(6000, t0);
        assert!(g.try_acquire(t0, 300).is_ok());
        assert_eq!(g.try_acquire(t0, 300), Err(Hold::InFlight));
        g.on_rate_limited(t0, None);
        assert_eq!(
            g.try_acquire(t0 + Duration::from_millis(1500), 300),
            Err(Hold::Cooldown)
        ); // doubled to 2 s
        assert!(g.try_acquire(t0 + Duration::from_millis(2100), 300).is_ok());
    }
}
