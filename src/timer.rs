//! Pomodoro phase machine. Pure state, ticked with an explicit `now` so it is
//! testable without sleeping.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Phase {
    Work,
    ShortBreak,
    LongBreak,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Work => "WORK",
            Phase::ShortBreak => "SHORT BREAK",
            Phase::LongBreak => "LONG BREAK",
        }
    }

    pub fn is_work(self) -> bool {
        self == Phase::Work
    }
}

#[derive(Clone, Copy)]
pub struct Settings {
    pub work: Duration,
    pub short_break: Duration,
    pub long_break: Duration,
    pub long_every: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            work: Duration::from_secs(25 * 60),
            short_break: Duration::from_secs(5 * 60),
            long_break: Duration::from_secs(15 * 60),
            long_every: 4,
        }
    }
}

pub struct Timer {
    pub phase: Phase,
    pub running: bool,
    pub remaining: Duration,
    pub settings: Settings,
    /// Work phases finished, used to decide when a long break is due.
    pub work_done: u32,
    pub pomodoros: u32,
    last: Instant,
}

impl Timer {
    pub fn new(settings: Settings) -> Self {
        Self {
            phase: Phase::Work,
            running: false,
            remaining: settings.work,
            settings,
            work_done: 0,
            pomodoros: 0,
            last: Instant::now(),
        }
    }

    pub fn total(&self) -> Duration {
        match self.phase {
            Phase::Work => self.settings.work,
            Phase::ShortBreak => self.settings.short_break,
            Phase::LongBreak => self.settings.long_break,
        }
    }

    /// Fraction of the current phase already elapsed, for the progress bar.
    pub fn progress(&self) -> f64 {
        let total = self.total().as_secs_f64();
        if total <= 0.0 {
            return 0.0;
        }
        (1.0 - self.remaining.as_secs_f64() / total).clamp(0.0, 1.0)
    }

    pub fn start(&mut self, now: Instant) {
        self.last = now;
        self.running = true;
    }

    pub fn pause(&mut self) {
        self.running = false;
    }

    fn switch(&mut self, phase: Phase) {
        self.phase = phase;
        self.remaining = self.total();
        self.running = false;
        self.last = Instant::now();
    }

    /// Returns true when the current phase just ran out.
    pub fn tick(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last);
        self.last = now;
        if !self.running {
            return false;
        }
        match self.remaining.checked_sub(elapsed) {
            Some(left) if !left.is_zero() => {
                self.remaining = left;
                false
            }
            _ => {
                self.remaining = Duration::ZERO;
                true
            }
        }
    }

    /// Moves on from a phase that just completed, counting pomodoros.
    pub fn complete(&mut self) {
        if self.phase.is_work() {
            self.work_done += 1;
            self.pomodoros += 1;
            let long = self.work_done.is_multiple_of(self.settings.long_every);
            let next = if long {
                Phase::LongBreak
            } else {
                Phase::ShortBreak
            };
            self.switch(next);
        } else {
            self.switch(Phase::Work);
        }
    }

    /// Throws the current phase away without earning a pomodoro for it.
    pub fn skip(&mut self) {
        let next = match self.phase {
            Phase::Work => Phase::ShortBreak,
            _ => Phase::Work,
        };
        self.switch(next);
    }

    /// Puts the current phase back to full length.
    pub fn reset(&mut self) {
        self.running = false;
        self.remaining = self.total();
        self.last = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quick() -> Settings {
        Settings {
            work: Duration::from_secs(10),
            short_break: Duration::from_secs(2),
            long_break: Duration::from_secs(30),
            long_every: 2,
        }
    }

    #[test]
    fn paused_timer_does_not_advance() {
        let mut t = Timer::new(quick());
        let t0 = Instant::now();
        assert!(!t.tick(t0 + Duration::from_secs(60)));
        assert_eq!(t.remaining, Duration::from_secs(10));
        assert_eq!(t.progress(), 0.0);
    }

    #[test]
    fn running_timer_completes_only_when_drained() {
        let mut t = Timer::new(quick());
        let t0 = Instant::now();
        t.start(t0);
        assert!(!t.tick(t0 + Duration::from_secs(4)));
        assert_eq!(t.remaining, Duration::from_secs(6));
        assert!(t.tick(t0 + Duration::from_secs(11)));
        assert_eq!(t.remaining, Duration::ZERO);
    }

    #[test]
    fn long_break_arrives_every_n_work_phases() {
        let mut t = Timer::new(quick());
        t.complete();
        assert_eq!(t.phase, Phase::ShortBreak);
        assert_eq!(t.pomodoros, 1);
        t.complete();
        assert_eq!(t.phase, Phase::Work);
        t.complete();
        assert_eq!(
            t.phase,
            Phase::LongBreak,
            "second pomodoro should earn the long break"
        );
        assert_eq!(t.remaining, Duration::from_secs(30));
        assert!(!t.running, "the next phase waits for the user to start it");
    }

    #[test]
    fn skipping_earns_no_pomodoro() {
        let mut t = Timer::new(quick());
        t.start(Instant::now());
        t.skip();
        assert_eq!(t.pomodoros, 0);
        assert_eq!(t.phase, Phase::ShortBreak);
    }
}
