//! The status-line message: one at a time, replaced by the next, gone after
//! its time to live.

use std::time::{Duration, Instant};

/// How long a toast stays on the status line.
pub const TOAST_TTL: Duration = Duration::from_secs(4);
/// An error stays on the status line longer.
pub const ERROR_TTL: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
    pub text: String,
    pub until: Instant,
    pub error: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Toasts {
    current: Option<Toast>,
}

impl Toasts {
    pub fn notify(&mut self, text: impl Into<String>) {
        self.set(text.into(), TOAST_TTL, false);
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.set(text.into(), ERROR_TTL, true);
    }

    fn set(&mut self, text: String, ttl: Duration, error: bool) {
        self.current = Some(Toast {
            text,
            until: Instant::now() + ttl,
            error,
        });
    }

    pub fn current(&self) -> Option<&Toast> {
        self.current.as_ref()
    }

    pub fn expire(&mut self, now: Instant) {
        if self.current.as_ref().is_some_and(|t| t.until <= now) {
            self.current = None;
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.current.as_ref().map(|t| t.until)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_toast_expires_and_an_error_lasts_longer() {
        let mut t = Toasts::default();
        t.notify("hi");
        let until = t.next_deadline().unwrap();
        t.expire(until - Duration::from_millis(1));
        assert_eq!(t.current().unwrap().text, "hi");
        t.expire(until);
        assert!(t.current().is_none());
        t.error("bad");
        assert!(t.current().unwrap().error);
        assert!(t.next_deadline().unwrap() > Instant::now() + TOAST_TTL);
    }
}
