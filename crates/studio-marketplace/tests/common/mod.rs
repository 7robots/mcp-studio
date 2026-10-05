#![allow(dead_code)]

use std::path::{Path, PathBuf};

use studio_core::config::MarketplaceConfig;
use studio_marketplace::Marketplace;

pub fn acme() -> Marketplace {
    let c: MarketplaceConfig = toml::from_str(
        r#"
id = "acme"
repo = "acme/acme-plugin-marketplace"
owner_name = "Acme Team"
"#,
    )
    .unwrap();
    Marketplace::from_config(&c, Some("acme-bot"))
}

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Copy a directory tree.
pub fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.path().is_dir() {
            copy_tree(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), dst).unwrap();
        }
    }
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A git repo with a committer identity, on `main`.
pub fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.name", "Acme Tester"]);
    git(dir, &["config", "user.email", "tester@acme.example"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}
