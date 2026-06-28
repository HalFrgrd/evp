use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

thread_local! {
    static TIMINGS: RefCell<HashMap<String, Duration>> = RefCell::new(HashMap::new());
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
        TIMINGS.with(|t| {
            t.borrow_mut().insert(self.name.to_string(), elapsed);
        });
    }
}

/// Retrieve the accumulated millisecond timings from the thread-local storage.
pub fn get_timings() -> HashMap<String, u128> {
    TIMINGS.with(|t| {
        t.borrow()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_millis()))
            .collect()
    })
}

/// Reset the thread-local timings registry.
pub fn clear_timings() {
    TIMINGS.with(|t| t.borrow_mut().clear());
}
