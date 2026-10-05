//! Lifecycle operations against throwaway git repos. No network: the remote
//! is a local bare repo and the GitHub API is a local mock.

mod common;

use std::path::Path;

use common::{acme, git, init_repo};
use studio_core::Secret;
use studio_core::check::Status;
use studio_core::config::PublishMode;
use studio_marketplace::model::{AuthType, Kind};
use studio_marketplace::ops::{self, EntryPatch};
use studio_marketplace::provision::{self, AdoptOutcome, ProvisionOptions};
use studio_marketplace::publish::{self, Gh, GithubApi, Published, Route};
use studio_marketplace::{draft, git as mgit, reconcile, validate_dir};

fn search_patch() -> EntryPatch {
    EntryPatch {
        name: Some("Acme Search".into()),
        description: Some("Search Acme's product documentation.".into()),
        url: Some("https://search.mcp.acme.example/mcp".into()),
        tags: Some(vec!["Search".into(), "docs".into(), "search".into()]),
        category: Some("Developer Tools".into()),
        version: Some("0.1.0".into()),
        ..Default::default()
    }
}

/// A provisioned, committed marketplace in `<tmp>/mkt`.
fn provisioned(tmp: &Path) -> std::path::PathBuf {
    let dir = tmp.join("mkt");
    init_repo(&dir);
    provision::provision(&dir, &acme(), ProvisionOptions { health: true }).unwrap();
    dir
}

fn all_pass(dir: &Path) {
    let bad: Vec<_> = reconcile(dir, &acme())
        .into_iter()
        .filter(|c| c.status != Status::Pass)
        .collect();
    assert!(bad.is_empty(), "{bad:#?}");
    let v = validate_dir(dir, &acme());
    assert!(v.is_ok(), "{:#?}", v.findings);
}

#[test]
fn provision_seeds_a_valid_empty_marketplace() {
    let t = tempfile::tempdir().unwrap();
    let dir = provisioned(t.path());
    for f in [
        ".claude-plugin/marketplace.json",
        ".agents/plugins/marketplace.json",
        "schema/server.schema.json",
        ".github/workflows/validate.yml",
        ".github/workflows/health.yml",
        "servers/.gitkeep",
        "plugins/.gitkeep",
        "README.md",
    ] {
        assert!(dir.join(f).is_file(), "{f}");
    }
    assert_eq!(git(&dir, &["log", "--format=%s"]), "seed marketplace");
    all_pass(&dir);
    assert!(validate_dir(&dir, &acme()).notes.is_empty());
    // Not empty any more: a second provision refuses.
    assert!(provision::provision(&dir, &acme(), ProvisionOptions::default()).is_err());
}

#[test]
fn add_update_deprecate_reinstate_remove() {
    let t = tempfile::tempdir().unwrap();
    let dir = provisioned(t.path());
    let m = acme();

    // add (dry run first: plans, writes nothing)
    let cs = ops::plan_add(
        &dir,
        &m,
        search_patch().into_entry(Kind::McpServer),
        Some("acme-search"),
    )
    .unwrap();
    assert_eq!(
        cs.paths(),
        [
            ".agents/plugins/marketplace.json",
            ".claude-plugin/marketplace.json",
            "servers/acme-search/.claude-plugin/plugin.json",
            "servers/acme-search/.codex-plugin/plugin.json",
            "servers/acme-search/.mcp.json",
            "servers/acme-search/README.md",
            "servers/acme-search/server.yaml",
        ]
    );
    assert!(cs.diff().contains("+name: Acme Search"));
    assert!(!dir.join("servers/acme-search").exists());
    ops::apply_checked(&dir, &m, &cs).unwrap();
    ops::commit(&dir, &cs).unwrap();
    assert_eq!(git(&dir, &["log", "-1", "--format=%s"]), "add: Acme Search");
    let yaml = std::fs::read_to_string(dir.join("servers/acme-search/server.yaml")).unwrap();
    assert!(yaml.starts_with("# Source of truth for this entry."));
    assert!(yaml.contains("tags:\n- search\n- docs\n"), "{yaml}");
    all_pass(&dir);

    // adding it again is refused
    let again = ops::plan_add(
        &dir,
        &m,
        search_patch().into_entry(Kind::McpServer),
        Some("acme-search"),
    );
    assert!(again.unwrap_err().to_string().contains("update"));
    // reserved slugs are refused
    assert!(
        ops::plan_add(
            &dir,
            &m,
            search_patch().into_entry(Kind::McpServer),
            Some("plugins")
        )
        .is_err()
    );

    // update: credentialed now → Codex-only MCP file appears, auth noted
    let patch = EntryPatch {
        auth: Some(AuthType::Bearer),
        version: Some("0.2.0".into()),
        ..Default::default()
    };
    let cs = ops::plan_update(&dir, &m, "acme-search", &patch).unwrap();
    assert!(
        cs.paths()
            .contains(&"servers/acme-search/.codex-plugin/mcp.json")
    );
    assert!(
        cs.notes
            .iter()
            .any(|n| n.starts_with("Auth changed: none -> bearer"))
    );
    ops::apply_checked(&dir, &m, &cs).unwrap();
    ops::commit(&dir, &cs).unwrap();
    let msg = git(&dir, &["log", "-1", "--format=%B"]);
    assert!(
        msg.starts_with("update: Acme Search\n\nAuth changed"),
        "{msg}"
    );
    all_pass(&dir);

    // and back to none: the Codex-only file is deleted
    let patch = EntryPatch {
        auth: Some(AuthType::None),
        ..Default::default()
    };
    let cs = ops::plan_update(&dir, &m, "acme-search", &patch).unwrap();
    let del = cs
        .changes
        .iter()
        .find(|c| c.path.ends_with(".codex-plugin/mcp.json"))
        .unwrap();
    assert_eq!(del.action(), "delete");
    assert!(cs.notes.iter().any(|n| n.contains("Version left at 0.2.0")));
    ops::apply_checked(&dir, &m, &cs).unwrap();
    ops::commit(&dir, &cs).unwrap();
    all_pass(&dir);

    // deprecate → banner, still listed
    let cs = ops::plan_deprecate(&dir, &m, "acme-search", "Use Acme Find.").unwrap();
    ops::apply_checked(&dir, &m, &cs).unwrap();
    ops::commit(&dir, &cs).unwrap();
    let readme = std::fs::read_to_string(dir.join("servers/acme-search/README.md")).unwrap();
    assert!(readme.contains("# Acme Search\n\n> **DEPRECATED.** Use Acme Find.\n"));
    assert!(ops::list(&dir)[0].deprecated);
    assert_eq!(
        git(&dir, &["log", "-1", "--format=%s"]),
        "deprecate: Acme Search"
    );
    all_pass(&dir);

    // reinstate
    let cs = ops::plan_reinstate(&dir, &m, "acme-search").unwrap();
    ops::apply_checked(&dir, &m, &cs).unwrap();
    ops::commit(&dir, &cs).unwrap();
    assert!(
        !std::fs::read_to_string(dir.join("servers/acme-search/server.yaml"))
            .unwrap()
            .contains("deprecated")
    );
    assert!(ops::plan_reinstate(&dir, &m, "acme-search").is_err());
    all_pass(&dir);

    // remove: directory and both catalog entries go, in one commit
    let cs = ops::plan_remove(&dir, &m, "acme-search").unwrap();
    ops::apply_checked(&dir, &m, &cs).unwrap();
    ops::commit(&dir, &cs).unwrap();
    assert!(!dir.join("servers/acme-search").exists());
    assert_eq!(git(&dir, &["status", "--porcelain"]), "");
    assert_eq!(
        git(&dir, &["log", "-1", "--format=%s"]),
        "remove: Acme Search"
    );
    let changed = git(&dir, &["show", "--name-only", "--format=", "HEAD"]);
    assert!(
        changed.contains(".claude-plugin/marketplace.json")
            && changed.contains(".agents/plugins/marketplace.json")
    );
    assert!(ops::list(&dir).is_empty());
    all_pass(&dir);
}

#[test]
fn an_invalid_change_is_rolled_back() {
    let t = tempfile::tempdir().unwrap();
    let dir = provisioned(t.path());
    let mut p = search_patch();
    p.url = Some("http://search.mcp.acme.example/mcp".into());
    let cs = ops::plan_add(
        &dir,
        &acme(),
        p.into_entry(Kind::McpServer),
        Some("acme-search"),
    )
    .unwrap();
    let err = ops::apply_checked(&dir, &acme(), &cs)
        .unwrap_err()
        .to_string();
    assert!(err.contains("nothing was written"), "{err}");
    assert!(!dir.join("servers/acme-search").exists());
    assert_eq!(git(&dir, &["status", "--porcelain"]), "");
}

#[test]
fn a_skill_entry_lands_in_plugins() {
    let t = tempfile::tempdir().unwrap();
    let dir = provisioned(t.path());
    let p = EntryPatch {
        name: Some("Acme Helper".into()),
        description: Some("Skills for Acme.".into()),
        ..Default::default()
    };
    let cs = ops::plan_add(&dir, &acme(), p.into_entry(Kind::Skill), None).unwrap();
    assert!(cs.paths().iter().all(|p| !p.ends_with(".mcp.json")));
    ops::apply_checked(&dir, &acme(), &cs).unwrap();
    assert!(dir.join("plugins/acme-helper/server.yaml").is_file());
    all_pass(&dir);
}

#[test]
fn workspaces_refuse_dirty_clones_and_clone_when_missing() {
    let t = tempfile::tempdir().unwrap();
    let dir = provisioned(t.path());
    let bare = t.path().join("remote.git");
    git(
        t.path(),
        &[
            "clone",
            "-q",
            "--bare",
            dir.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    let url = format!("file://{}", bare.display());

    std::fs::write(dir.join("stray.txt"), "x").unwrap();
    let e = mgit::open_workspace(&dir, &url, "main", true).unwrap_err();
    assert!(e.to_string().contains("uncommitted"), "{e}");
    std::fs::remove_file(dir.join("stray.txt")).unwrap();
    let ws = mgit::open_workspace(&dir, &url, "main", true).unwrap();
    assert!(!ws.is_temporary());

    let missing = t.path().join("not-here");
    assert!(
        mgit::open_workspace(&missing, &url, "main", false)
            .unwrap_err()
            .to_string()
            .contains("not cloned")
    );
    let ws = mgit::open_workspace(&missing, &url, "main", true).unwrap();
    assert!(ws.is_temporary());
    assert!(ws.dir.join(".claude-plugin/marketplace.json").is_file());
}

/// A local stand-in for `GET /repos/{o}/{r}/branches/{b}`.
async fn mock_api(protected: bool) -> String {
    use axum::{Json, Router, routing::get};
    let app =
        Router::new().route(
            "/repos/acme/acme-plugin-marketplace/branches/main",
            get(move || async move {
                Json(serde_json::json!({"name": "main", "protected": protected}))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// A provisioned clone whose origin is a local bare repo.
fn with_remote(tmp: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let seed = provisioned(tmp);
    let bare = tmp.join("remote.git");
    git(
        tmp,
        &[
            "clone",
            "-q",
            "--bare",
            seed.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    let work = tmp.join("work");
    git(
        tmp,
        &[
            "clone",
            "-q",
            bare.to_str().unwrap(),
            work.to_str().unwrap(),
        ],
    );
    git(&work, &["config", "user.name", "Acme Tester"]);
    git(&work, &["config", "user.email", "tester@acme.example"]);
    git(&work, &["config", "commit.gpgsign", "false"]);
    (work, bare)
}

#[tokio::test]
async fn auto_publish_pushes_to_an_unprotected_branch() {
    let t = tempfile::tempdir().unwrap();
    let (work, bare) = with_remote(t.path());
    let m = acme();
    let cs = ops::plan_add(
        &work,
        &m,
        search_patch().into_entry(Kind::McpServer),
        Some("acme-search"),
    )
    .unwrap();
    ops::apply_checked(&work, &m, &cs).unwrap();
    let commit = ops::commit(&work, &cs).unwrap();

    let api = GithubApi::with_base(&mock_api(false).await, Secret::new("test-token"));
    let route = publish::decide(PublishMode::Auto, Some(&api), &m.repo, "main")
        .await
        .unwrap();
    assert_eq!(route, Route::Push);
    let gh = Gh {
        program: "/nonexistent/gh".into(),
        token: None,
    };
    let out = publish::publish(&work, "main", &m.repo, &cs, &commit, &route, &gh).unwrap();
    assert_eq!(
        out,
        Published::Pushed {
            branch: "main".into()
        }
    );
    assert_eq!(
        git(&bare, &["log", "-1", "--format=%s", "main"]),
        "add: Acme Search"
    );
}

#[tokio::test]
async fn auto_publish_opens_a_pr_on_a_protected_branch() {
    let t = tempfile::tempdir().unwrap();
    let (work, bare) = with_remote(t.path());
    let m = acme();
    let cs = ops::plan_add(
        &work,
        &m,
        search_patch().into_entry(Kind::McpServer),
        Some("acme-search"),
    )
    .unwrap();
    ops::apply_checked(&work, &m, &cs).unwrap();
    let commit = ops::commit(&work, &cs).unwrap();

    let api = GithubApi::with_base(&mock_api(true).await, Secret::new("test-token"));
    let route = publish::decide(PublishMode::Auto, Some(&api), &m.repo, "main")
        .await
        .unwrap();
    assert!(matches!(route, Route::PullRequest { .. }));

    // A fake `gh` that records its arguments and prints a PR URL.
    let log = t.path().join("gh-args");
    let fake = t.path().join("gh");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\necho https://github.example/acme/pull/7\n",
            log.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let gh = Gh {
        program: fake.display().to_string(),
        token: None,
    };
    let out = publish::publish(&work, "main", &m.repo, &cs, &commit, &route, &gh).unwrap();
    assert_eq!(
        out,
        Published::PullRequest {
            branch: "add/acme-search".into(),
            url: "https://github.example/acme/pull/7".into()
        }
    );
    let args = std::fs::read_to_string(&log).unwrap();
    assert!(
        args.contains("--head\nadd/acme-search\n") && args.contains("--title\nadd: Acme Search\n"),
        "{args}"
    );
    // The branch is on the remote; main is untouched there and locally.
    assert_eq!(
        git(&bare, &["log", "-1", "--format=%s", "add/acme-search"]),
        "add: Acme Search"
    );
    assert_eq!(
        git(&bare, &["log", "-1", "--format=%s", "main"]),
        "seed marketplace"
    );
    assert_eq!(
        git(&work, &["log", "-1", "--format=%s", "main"]),
        "seed marketplace"
    );
}

#[tokio::test]
async fn explicit_modes_skip_the_api() {
    assert_eq!(
        publish::decide(PublishMode::Push, None, "a/b", "main")
            .await
            .unwrap(),
        Route::Push
    );
    assert!(matches!(
        publish::decide(PublishMode::Pr, None, "a/b", "main")
            .await
            .unwrap(),
        Route::PullRequest { .. }
    ));
    assert!(
        publish::decide(PublishMode::Auto, None, "a/b", "main")
            .await
            .is_err()
    );
}

#[test]
fn adopt_seeds_empty_repos_adds_tooling_and_refuses_strangers() {
    let t = tempfile::tempdir().unwrap();
    let m = acme();

    let empty = t.path().join("empty");
    init_repo(&empty);
    std::fs::write(empty.join("README.md"), "# ours\n").unwrap();
    git(&empty, &["add", "-A"]);
    git(&empty, &["commit", "-q", "-m", "init"]);
    let out = provision::adopt(&empty, &m, ProvisionOptions::default()).unwrap();
    assert!(matches!(out, AdoptOutcome::Seeded { .. }));
    assert_eq!(
        std::fs::read_to_string(empty.join("README.md")).unwrap(),
        "# ours\n"
    );
    assert!(validate_dir(&empty, &m).is_ok());

    // An older marketplace: catalogs, no schema/ → tooling added, entries untouched.
    let old = t.path().join("old");
    init_repo(&old);
    common::copy_tree(&common::fixtures().join("acme"), &old);
    git(&old, &["add", "-A"]);
    git(&old, &["commit", "-q", "-m", "init"]);
    match provision::adopt(&old, &m, ProvisionOptions::default()).unwrap() {
        AdoptOutcome::AddedTooling { files } => {
            assert!(files.contains(&"schema/server.schema.json".to_string()));
            assert!(files.contains(&".github/workflows/validate.yml".to_string()));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        provision::adopt(&old, &m, ProvisionOptions::default()).unwrap(),
        AdoptOutcome::NothingToDo
    );

    let stranger = t.path().join("stranger");
    init_repo(&stranger);
    std::fs::write(stranger.join("main.go"), "package main\n").unwrap();
    assert!(provision::adopt(&stranger, &m, ProvisionOptions::default()).is_err());
}

#[test]
fn drafts_an_entry_from_a_fleet_repo() {
    let t = tempfile::tempdir().unwrap();
    let repo = t.path().join("weather-mcp-worker");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("wrangler.toml"),
        "name = \"weather-mcp-worker\"\n[vars]\nPUBLIC_MCP_URL = \"https://weather.mcp.acme.example/mcp\"\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("package.json"),
        r#"{"name": "weather-mcp-worker", "version": "1.4.0", "description": "pkg"}"#,
    )
    .unwrap();
    std::fs::write(
        repo.join("src/mcp.ts"),
        "const INSTRUCTIONS =\n  \"Forecasts for Acme sites. \" +\n  \"Ask by site name.\";\nnew McpServer({ name: \"w\" }, { instructions: INSTRUCTIONS });\n",
    )
    .unwrap();
    let d = draft::entry_from_server(&repo, Some("acme-weather")).unwrap();
    assert_eq!(d.entry.name, "Acme Weather");
    assert_eq!(
        d.entry.url.as_deref(),
        Some("https://weather.mcp.acme.example/mcp")
    );
    assert_eq!(
        d.entry.description,
        "Forecasts for Acme sites. Ask by site name."
    );
    assert_eq!(d.entry.version.as_deref(), Some("1.4.0"));
    assert_eq!(d.entry.auth_type(), AuthType::Oauth);

    // Without instructions, package.json's description; slug from the directory.
    std::fs::remove_file(repo.join("src/mcp.ts")).unwrap();
    let d = draft::entry_from_server(&repo, None).unwrap();
    assert_eq!(d.entry.slug.as_deref(), Some("weather-mcp-worker"));
    assert_eq!(d.entry.description, "pkg");
}
