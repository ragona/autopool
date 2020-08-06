use std::{
    fmt::Debug,
    time::{Duration, Instant},
};

#[derive(Debug, Copy, Clone)]
pub struct TickWorkTracker {
    pub start: Instant,
    pub last: Instant,
    pub rate: Duration,
    pub all_work: usize,
    pub tick_work: usize,
}

pub struct Tick {
    pub start: Instant,
    pub work: usize,
}

impl TickWorkTracker {
    pub fn new(rate: Duration) -> Self {
        Self { rate, start: Instant::now(), last: Instant::now(), all_work: 0, tick_work: 0 }
    }

    /// Time remaining in current tick (can be negative)
    pub fn secs_remaining_in_tick(&self) -> f32 {
        self.rate.as_secs_f32() - self.last.elapsed().as_secs_f32()
    }

    pub fn tick_done(&self) -> bool {
        self.secs_remaining_in_tick() < 0.0
    }

    pub fn track_work(&mut self) {
        self.all_work += 1;
        self.tick_work += 1;
    }

    pub fn reset_tick(&mut self) {
        self.last = Instant::now();
        self.tick_work = 0;
    }

    pub fn tick_rate_per_sec(&self) -> f32 {
        self.tick_work as f32 / self.last.elapsed().as_secs_f32()
    }

    pub fn overall_rate_per_sec(&self) -> f32 {
        self.all_work as f32 / self.start.elapsed().as_secs_f32()
    }
}

impl Default for Tick {
    fn default() -> Self {
        Self { start: Instant::now(), work: 0 }
    }
}
