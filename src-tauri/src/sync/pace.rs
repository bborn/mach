//! Spacing `messages.get` calls so one account stays inside Gmail's quota.
//!
//! The quota is a rate (units per user per minute), and concurrency cannot
//! express a rate: two requests in flight is four a second at a 400 ms round
//! trip and thirteen a second at 150 ms. `backfill_fetch_concurrency` was sized
//! on the 400 ms guess, and when Gmail answered faster the account spent its
//! whole minute's budget in seconds. A backlog of 1,339 messages on the owner's
//! personal account (27,000 units at 20 a fetch) then hit the limit on every
//! pass, and that account received no mail for five days.
//!
//! So each request waits for a slot. Slots are `60s / per_minute` apart, handed
//! out in order, and a slot that has already gone by is not saved up: an idle
//! account does not earn a burst.

use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

pub struct Pacer {
    spacing: Duration,
    next: Mutex<Option<Instant>>,
}

impl Pacer {
    /// `per_minute == 0` means unpaced.
    pub fn per_minute(per_minute: u32) -> Self {
        let spacing = if per_minute == 0 {
            Duration::ZERO
        } else {
            Duration::from_secs(60) / per_minute
        };
        Self {
            spacing,
            next: Mutex::new(None),
        }
    }

    /// Wait for this request's slot.
    pub async fn wait(&self) {
        if self.spacing.is_zero() {
            return;
        }
        let slot = {
            let mut next = self.next.lock().await;
            let now = Instant::now();
            let slot = next.map_or(now, |n| n.max(now));
            *next = Some(slot + self.spacing);
            slot
        };
        tokio::time::sleep_until(slot).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn requests_are_spaced_by_the_budget() {
        let pacer = Pacer::per_minute(240); // one every 250 ms
        let start = Instant::now();
        for _ in 0..5 {
            pacer.wait().await;
        }
        // The first goes at once, the next four a slot apart.
        assert_eq!(start.elapsed(), Duration::from_millis(1_000));
    }

    #[tokio::test(start_paused = true)]
    async fn an_idle_account_does_not_bank_a_burst() {
        let pacer = Pacer::per_minute(240);
        pacer.wait().await;
        tokio::time::sleep(Duration::from_secs(10)).await;
        let start = Instant::now();
        pacer.wait().await;
        pacer.wait().await;
        assert_eq!(start.elapsed(), Duration::from_millis(250));
    }

    #[tokio::test(start_paused = true)]
    async fn zero_is_unpaced() {
        let pacer = Pacer::per_minute(0);
        let start = Instant::now();
        for _ in 0..100 {
            pacer.wait().await;
        }
        assert_eq!(start.elapsed(), Duration::ZERO);
    }
}
