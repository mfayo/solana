#![allow(clippy::integer_arithmetic)]
pub mod counter;
pub mod datapoint;
mod metrics;
pub use crate::metrics::{flush, query, set_host_id, set_panic_hook, submit};

use std::sync::Arc;

/// A helper that sends the count of created tokens as a datapoint.
pub struct TokenCounter(Arc<&'static str>);

impl TokenCounter {
    /// Creates a new counter with the specified metrics `name`.
    pub fn new(name: &'static str) -> Self {
        Self(Arc::new(name))
    }

    /// Creates a new token for this counter. The metric's value will be equal
    /// to the number of `CounterToken`s.
    pub fn create_token(&self) -> CounterToken {
        // new_count = strong_count
        //    - 1 (in TokenCounter)
        //    + 1 (token that's being created)
        datapoint_info!(*self.0, ("count", Arc::strong_count(&self.0), i64));
        CounterToken(self.0.clone())
    }
}

/// A token for `TokenCounter`.
pub struct CounterToken(Arc<&'static str>);

impl Clone for CounterToken {
    fn clone(&self) -> Self {
        // new_count = strong_count
        //    - 1 (in TokenCounter)
        //    + 1 (token that's being created)
        datapoint_info!(*self.0, ("count", Arc::strong_count(&self.0), i64));
        CounterToken(self.0.clone())
    }
}

impl Drop for CounterToken {
    fn drop(&mut self) {
        // new_count = strong_count
        //    - 1 (in TokenCounter)
        //    - 1 (token that's being dropped)
        datapoint_info!(*self.0, ("count", Arc::strong_count(&self.0) - 2, i64));
    }
}

impl Drop for TokenCounter {
    fn drop(&mut self) {
        datapoint_info!(*self.0, ("count", Arc::strong_count(&self.0) - 2, i64));
    }
}
