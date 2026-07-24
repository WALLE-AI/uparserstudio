//! `MockDispatch`: a keyed queue of pre-seeded responses, standing in for
//! `ParseCtx::dispatch` so a multi-round adapter's orchestration (P1's
//! two-stage protocols) can be exercised fully offline. Per §5.3/T-0.9.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
pub struct MockDispatch {
    responses: Mutex<HashMap<String, Vec<Value>>>,
}

impl MockDispatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a response to be returned the next time `key` is dispatched.
    pub fn seed(&self, key: &str, response: Value) {
        self.responses
            .lock()
            .expect("mutex not poisoned")
            .entry(key.to_string())
            .or_default()
            .push(response);
    }

    /// Pop the next seeded response for `key`, in FIFO order.
    pub fn dispatch(&self, key: &str) -> Option<Value> {
        let mut responses = self.responses.lock().expect("mutex not poisoned");
        let queue = responses.get_mut(key)?;
        if queue.is_empty() {
            None
        } else {
            Some(queue.remove(0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_and_pops_in_fifo_order() {
        let mock = MockDispatch::new();
        mock.seed("layout", serde_json::json!({"stage": 1}));
        mock.seed("layout", serde_json::json!({"stage": 2}));

        assert_eq!(
            mock.dispatch("layout"),
            Some(serde_json::json!({"stage": 1}))
        );
        assert_eq!(
            mock.dispatch("layout"),
            Some(serde_json::json!({"stage": 2}))
        );
        assert_eq!(mock.dispatch("layout"), None);
    }

    #[test]
    fn unknown_key_returns_none() {
        let mock = MockDispatch::new();
        assert_eq!(mock.dispatch("nope"), None);
    }
}
