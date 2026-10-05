//! Small helpers for the external tools Studio drives (`gh`, `git`).

use std::path::Path;
use std::process::Command;

use crate::secret::Secret;

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("`{0}` is not installed or not on PATH")]
    Missing(String),
    #[error("`{cmd}` failed: {stderr}")]
    Failed { cmd: String, stderr: String },
}

/// Run a command and return trimmed stdout.
pub fn run(program: &str, args: &[&str], cwd: Option<&Path>) -> Result<String, ExecError> {
    let mut c = Command::new(program);
    c.args(args);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let out = c.output().map_err(|_| ExecError::Missing(program.into()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        Err(ExecError::Failed {
            cmd: format!("{program} {}", args.join(" ")),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        })
    }
}

/// A GitHub token for `account` (or the active `gh` account), via `gh auth token`.
/// `GH_TOKEN`/`GITHUB_TOKEN` win when set, so CI and tests need no `gh`.
pub fn github_token(account: Option<&str>) -> Result<Secret, ExecError> {
    for var in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(t) = std::env::var(var)
            && !t.is_empty()
        {
            return Ok(Secret::new(t));
        }
    }
    let mut args = vec!["auth", "token"];
    if let Some(a) = account {
        args.extend(["-u", a]);
    }
    run("gh", &args, None)
        .map(Secret::new)
        .map_err(|e| match e {
            // Never echo gh's output: it is about a credential.
            ExecError::Failed { cmd, .. } => ExecError::Failed {
                cmd,
                stderr: format!("no token for gh account {}", account.unwrap_or("(active)")),
            },
            other => other,
        })
}
