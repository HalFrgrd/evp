use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct TimingStats {
    pub total_ms: u128,
    pub count: u32,
}

static TIMINGS: OnceLock<Mutex<HashMap<String, (Duration, u32)>>> = OnceLock::new();

fn get_timings_map() -> &'static Mutex<HashMap<String, (Duration, u32)>> {
    TIMINGS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// RAII helper that logs duration and records timing when dropped.
pub struct ScopeTimer {
    name: &'static str,
    start: Instant,
}

impl ScopeTimer {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            start: Instant::now(),
        }
    }
}

impl Drop for ScopeTimer {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed();
        tracing::debug!(
            scope = self.name,
            elapsed_ms = elapsed.as_millis(),
            "scope finished"
        );
        if let Ok(mut map) = get_timings_map().lock() {
            let entry = map
                .entry(self.name.to_string())
                .or_insert((Duration::ZERO, 0));
            entry.0 += elapsed;
            entry.1 += 1;
        }
    }
}

/// Retrieve the accumulated millisecond timings from the thread-local storage.
pub fn get_timings() -> HashMap<String, TimingStats> {
    if let Ok(map) = get_timings_map().lock() {
        map.iter()
            .map(|(k, &(dur, count))| {
                (
                    k.clone(),
                    TimingStats {
                        total_ms: dur.as_millis(),
                        count,
                    },
                )
            })
            .collect()
    } else {
        HashMap::new()
    }
}

/// Reset the thread-local timings registry.
pub fn clear_timings() {
    if let Ok(mut map) = get_timings_map().lock() {
        map.clear();
    }
}

pub static SUSPEND_LOGGING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub static RECORDING_LOGS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
