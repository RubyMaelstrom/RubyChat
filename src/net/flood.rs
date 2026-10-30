//! Client-side flood control: the penalty timer of RFC 1459 §8.10 ("Flood
//! control of clients") run on our own lines, so servers never throttle or
//! disconnect us. The timer advances two seconds per line and may run ten
//! seconds ahead of the clock: a burst of five lines goes at once, then one
//! line every two seconds.

use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct Flood {
    timer: Option<Instant>,
    penalty: Duration,
    window: Duration,
}

impl Default for Flood {
    fn default() -> Self {
        Self::new(Duration::from_secs(2), Duration::from_secs(10))
    }
}

impl Flood {
    pub fn new(penalty: Duration, window: Duration) -> Self {
        Self {
            timer: None,
            penalty,
            window,
        }
    }

    /// When the next line may be sent: when charging it keeps the timer
    /// within the window.
    pub fn ready_at(&self, now: Instant) -> Instant {
        match self.timer {
            Some(timer) if timer + self.penalty > now + self.window => {
                timer + self.penalty - self.window
            }
            _ => now,
        }
    }

    /// Charges one line sent at `now`.
    pub fn charge(&mut self, now: Instant) {
        let timer = self.timer.filter(|&timer| timer > now).unwrap_or(now);
        self.timer = Some(timer + self.penalty);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_lines_burst_then_one_every_two_seconds() {
        let start = Instant::now();
        let mut flood = Flood::default();
        for _ in 0..5 {
            assert_eq!(flood.ready_at(start), start);
            flood.charge(start);
        }
        // The sixth waits until its penalty fits within ten seconds.
        let sixth = flood.ready_at(start);
        assert_eq!(sixth, start + Duration::from_secs(2));
        flood.charge(sixth);
        assert_eq!(flood.ready_at(sixth), sixth + Duration::from_secs(2));
        // Idle time pays the penalty off.
        let later = start + Duration::from_secs(60);
        assert_eq!(flood.ready_at(later), later);
    }
}
