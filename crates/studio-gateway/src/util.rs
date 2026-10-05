//! Small helpers shared by the client, the fake gateway and the TUI.

use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Seconds since the Unix epoch, the unit the gateway uses for timestamps.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A timestamp as an age: `45s ago`, `12m ago`, `3h ago`, `2d ago`; `-` for none.
pub fn ago(ts: Option<u64>, now: u64) -> String {
    let Some(ts) = ts else { return "-".into() };
    let secs = now.saturating_sub(ts);
    match secs {
        0..60 => format!("{secs}s ago"),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

/// `bytes` of OS randomness, base64url without padding (PKCE verifiers, OAuth
/// `state`, the fake gateway's tokens).
pub fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::fill(&mut buf[..]);
    URL_SAFE_NO_PAD.encode(buf)
}

/// Writes through a sibling temp file and a rename, so a reader never sees a
/// half-written file. `private` restricts the file to its owner (token files).
pub fn write_atomic(path: &Path, text: &str, private: bool) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!("{name}.{}.tmp", std::process::id()));
    // Created exclusively with its final mode: never readable by others, and a
    // symlink planted at the temp path is not followed.
    let _ = std::fs::remove_file(&tmp);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(if private { 0o600 } else { 0o644 })
        .open(&tmp)?;
    std::io::Write::write_all(&mut file, text.as_bytes())?;
    drop(file);
    std::fs::rename(&tmp, path)
}

/// At most `max` characters of `text`, with `...` when cut.
pub fn truncate(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((i, _)) => format!("{}...", &text[..i]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_tokens_are_url_safe_and_distinct() {
        let a = random_token(32);
        assert_eq!(a.len(), 43);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert_ne!(a, random_token(32));
    }

    #[test]
    fn ages_pick_the_largest_whole_unit() {
        assert_eq!(ago(None, 100), "-");
        assert_eq!(ago(Some(100), 145), "45s ago");
        assert_eq!(ago(Some(0), 3599), "59m ago");
        assert_eq!(ago(Some(0), 7200), "2h ago");
        assert_eq!(ago(Some(0), 86400 * 3), "3d ago");
        assert_eq!(ago(Some(200), 100), "0s ago");
    }

    #[test]
    fn write_atomic_private_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/tokens.json");
        write_atomic(&path, "{}", true).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn truncate_counts_characters() {
        assert_eq!(truncate("héllo", 2), "hé...");
        assert_eq!(truncate("hi", 5), "hi");
    }
}
