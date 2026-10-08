//! Shared engine, monotonic clock, and deadline timer.
use crate::engine::Engine;
use common::{seed::SeedConfig, signing::unix_millis};
use std::{
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

const DEADLINE_POLL_INTERVAL: Duration = Duration::from_millis(5);

pub struct App {
    engine: Mutex<Engine>,
    epoch_ms: u64,
    started: Instant,
}
impl App {
    pub fn new(seed: SeedConfig, accept_secs: u64) -> anyhow::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            engine: Mutex::new(Engine::new(seed, accept_secs)?),
            epoch_ms: unix_millis(),
            started: Instant::now(),
        }))
    }
    /// Advance wall-clock timestamps using a monotonic clock.
    pub fn now(&self) -> u64 {
        self.epoch_ms + self.started.elapsed().as_millis() as u64
    }
    /// Bring deadlines up to date before exposing mutable trading state.
    pub fn lock(&self) -> MutexGuard<'_, Engine> {
        let mut engine = self.engine.lock().expect("exchange state poisoned");
        engine.advance(self.now());
        engine
    }
}

pub(crate) fn start_deadline_timer(app: &Arc<App>) {
    // Do not keep the server alive just to run its timer.
    let weak = Arc::downgrade(app);
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(DEADLINE_POLL_INTERVAL);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            let Some(app) = weak.upgrade() else {
                break;
            };
            drop(app.lock());
        }
    });
}
