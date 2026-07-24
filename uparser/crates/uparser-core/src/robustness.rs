//! Degenerate-output detection and temperature-escalating retry, per
//! ARCHITECTURE.md §9.2 / T-1.7. This is this project's own robustness
//! layer (not a port of MinerU's) — meant to be shared across future
//! generative-protocol adapters, since a client-side retry loop is a
//! reasonable belt-and-suspenders addition even where the server already
//! does repeat-avoidance (e.g. mineru-vlm's `no_repeat_ngram_size`).

/// Detect a degenerate response: a short substring repeating enough
/// times to dominate the output. A simple sliding-window heuristic —
/// good enough to catch the classic "model gets stuck looping a phrase"
/// failure mode without needing a full language model.
pub fn is_degenerate(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n < 12 {
        return false;
    }

    let max_period = (n / 4).clamp(1, 40);
    for period in 1..=max_period {
        let mut i = 0;
        while i + period * 2 <= n {
            if chars[i..i + period] == chars[i + period..i + period * 2] {
                let mut repeats = 2;
                let mut j = i + period * 2;
                while j + period <= n && chars[j - period..j] == chars[i..i + period] {
                    repeats += 1;
                    j += period;
                }
                let covered = repeats * period;
                if repeats >= 5 && covered * 10 >= n * 6 {
                    return true;
                }
                i = j.max(i + 1);
            } else {
                i += 1;
            }
        }
    }
    false
}

#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub temperature_step: f32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            temperature_step: 0.2,
        }
    }
}

/// Repeatedly call `attempt_fn(temperature)` until it returns a
/// non-degenerate result or `policy.max_attempts` is exhausted, raising
/// the temperature by `temperature_step` each retry. Always terminates
/// (returns the last attempt's output on exhaustion) rather than looping
/// forever.
pub async fn retry_with_temperature<F, Fut>(
    policy: &RetryPolicy,
    base_temperature: f32,
    mut attempt_fn: F,
) -> String
where
    F: FnMut(f32) -> Fut,
    Fut: std::future::Future<Output = String>,
{
    let mut temperature = base_temperature;
    let mut last = String::new();
    for _ in 0..policy.max_attempts.max(1) {
        last = attempt_fn(temperature).await;
        if !is_degenerate(&last) {
            return last;
        }
        temperature += policy.temperature_step;
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn normal_text_is_not_degenerate() {
        assert!(!is_degenerate(
            "The quick brown fox jumps over the lazy dog repeatedly, but each sentence differs."
        ));
    }

    #[test]
    fn repeated_phrase_is_degenerate() {
        let text = "the cat sat ".repeat(10);
        assert!(is_degenerate(&text));
    }

    #[test]
    fn short_text_is_never_flagged() {
        assert!(!is_degenerate("ab"));
    }

    #[tokio::test]
    async fn retries_until_non_degenerate() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let calls_clone = Rc::clone(&calls);
        let policy = RetryPolicy {
            max_attempts: 5,
            temperature_step: 0.1,
        };

        let result = retry_with_temperature(&policy, 0.0, move |temp| {
            calls_clone.borrow_mut().push(temp);
            let attempt = calls_clone.borrow().len();
            async move {
                if attempt < 3 {
                    "loop loop loop loop loop loop loop loop loop loop ".to_string()
                } else {
                    "a well formed sentence".to_string()
                }
            }
        })
        .await;

        assert_eq!(result, "a well formed sentence");
        assert_eq!(calls.borrow().len(), 3);
        assert!((calls.borrow()[2] - 0.2).abs() < 1e-6);
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts_without_hanging() {
        let policy = RetryPolicy {
            max_attempts: 3,
            temperature_step: 0.1,
        };
        let calls = Rc::new(RefCell::new(0u32));
        let calls_clone = Rc::clone(&calls);

        let result = retry_with_temperature(&policy, 0.0, move |_temp| {
            *calls_clone.borrow_mut() += 1;
            async move { "loop loop loop loop loop loop loop loop loop loop ".to_string() }
        })
        .await;

        assert!(is_degenerate(&result));
        assert_eq!(*calls.borrow(), 3);
    }

    #[tokio::test]
    async fn first_try_success_returns_immediately() {
        let policy = RetryPolicy::default();
        let calls = Rc::new(RefCell::new(0u32));
        let calls_clone = Rc::clone(&calls);

        let result = retry_with_temperature(&policy, 0.0, move |_temp| {
            *calls_clone.borrow_mut() += 1;
            async move { "a perfectly normal response".to_string() }
        })
        .await;

        assert_eq!(result, "a perfectly normal response");
        assert_eq!(*calls.borrow(), 1);
    }
}
