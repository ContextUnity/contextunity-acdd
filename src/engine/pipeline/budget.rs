use super::*;
use std::sync::Condvar;

#[derive(Default)]
pub(super) struct ByteBudgetState {
    pub(super) in_flight_bytes: u64,
    pub(super) peak_in_flight_bytes: u64,
    pub(super) max_item_bytes: u64,
    pub(super) oversized_admissions: u64,
}

pub(super) struct ByteBudget {
    limit: u64,
    state: Mutex<ByteBudgetState>,
    available: Condvar,
}

impl ByteBudget {
    pub(super) fn new(limit: u64) -> Arc<Self> {
        Arc::new(Self {
            limit: limit.max(1),
            state: Mutex::new(ByteBudgetState::default()),
            available: Condvar::new(),
        })
    }

    pub(super) fn acquire(self: &Arc<Self>, bytes: u64, cancelled: &AtomicBool) -> Result<MemoryPermit> {
        let bytes = bytes.max(1);
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            if cancelled.load(Ordering::Acquire) {
                bail!("cold pipeline cancelled while waiting for memory budget");
            }
            let oversized = bytes > self.limit;
            let admitted = if oversized {
                state.in_flight_bytes == 0
            } else {
                state.in_flight_bytes <= self.limit.saturating_sub(bytes)
            };
            if admitted {
                state.in_flight_bytes = state.in_flight_bytes.saturating_add(bytes);
                state.peak_in_flight_bytes = state.peak_in_flight_bytes.max(state.in_flight_bytes);
                state.max_item_bytes = state.max_item_bytes.max(bytes);
                if oversized {
                    state.oversized_admissions = state.oversized_admissions.saturating_add(1);
                }
                return Ok(MemoryPermit {
                    budget: Arc::clone(self),
                    bytes,
                });
            }
            state = self
                .available
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
    }

    fn release(&self, bytes: u64) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.in_flight_bytes = state.in_flight_bytes.saturating_sub(bytes);
        self.available.notify_all();
    }

    pub(super) fn wake_waiters(&self) {
        self.available.notify_all();
    }

    pub(super) fn snapshot(&self) -> ByteBudgetState {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

impl Clone for ByteBudgetState {
    fn clone(&self) -> Self {
        Self {
            in_flight_bytes: self.in_flight_bytes,
            peak_in_flight_bytes: self.peak_in_flight_bytes,
            max_item_bytes: self.max_item_bytes,
            oversized_admissions: self.oversized_admissions,
        }
    }
}

pub(super) struct MemoryPermit {
    budget: Arc<ByteBudget>,
    bytes: u64,
}

impl Drop for MemoryPermit {
    fn drop(&mut self) {
        self.budget.release(self.bytes);
    }
}

