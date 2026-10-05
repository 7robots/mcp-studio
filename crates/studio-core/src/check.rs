//! One result type for every check Studio runs — pattern conformance, live
//! probes, marketplace reconciliation — so the TUI and `--json` output render
//! them all the same way.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Conforms.
    Pass,
    /// Worth a look; not a failure (e.g. optional file missing).
    Warn,
    /// Does not conform, or a probe found something broken.
    Fail,
    /// Could not run (source disabled, not configured, unreachable).
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    /// Stable dotted id, e.g. `pattern.pins`, `http.unauth_401`, `cf.build`.
    pub id: String,
    pub status: Status,
    /// One line for a table cell or list row.
    pub summary: String,
    /// Supporting detail: expected vs actual, a diff excerpt, a URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

impl Check {
    pub fn new(id: impl Into<String>, status: Status, summary: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            status,
            summary: summary.into(),
            evidence: None,
        }
    }
    pub fn pass(id: impl Into<String>, summary: impl Into<String>) -> Self {
        Self::new(id, Status::Pass, summary)
    }
    pub fn warn(id: impl Into<String>, summary: impl Into<String>) -> Self {
        Self::new(id, Status::Warn, summary)
    }
    pub fn fail(id: impl Into<String>, summary: impl Into<String>) -> Self {
        Self::new(id, Status::Fail, summary)
    }
    pub fn skip(id: impl Into<String>, summary: impl Into<String>) -> Self {
        Self::new(id, Status::Skip, summary)
    }
    pub fn with_evidence(mut self, evidence: impl Into<String>) -> Self {
        self.evidence = Some(evidence.into());
        self
    }
}

/// The worst status in a set, ignoring skips unless everything skipped.
pub fn rollup<'a>(checks: impl IntoIterator<Item = &'a Check>) -> Status {
    let mut worst = None;
    let mut any = false;
    for c in checks {
        any = true;
        if c.status != Status::Skip {
            worst = worst.max(Some(c.status));
        }
    }
    match (worst, any) {
        (Some(s), _) => s,
        (None, true) => Status::Skip,
        (None, false) => Status::Pass,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollup_takes_the_worst_non_skip() {
        let cs = [
            Check::pass("a", ""),
            Check::skip("b", ""),
            Check::warn("c", ""),
        ];
        assert_eq!(rollup(&cs), Status::Warn);
        assert_eq!(rollup(&[Check::skip("a", "")]), Status::Skip);
        assert_eq!(
            rollup(&[Check::fail("a", ""), Check::pass("b", "")]),
            Status::Fail
        );
        assert_eq!(rollup(&[]), Status::Pass);
    }
}
