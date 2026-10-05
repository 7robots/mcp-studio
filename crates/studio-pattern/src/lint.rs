//! Static rules a conforming repo follows, read from the pack's manifest.
//!
//! Everything here reads files in the repo checkout; nothing runs `npm`, talks
//! to the network, or needs the instance's values. Each rule reports one
//! [`Check`] under a stable id, so the TUI and `--json` can track it over time.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use regex::Regex;
use serde_json::Value as Json;
use studio_core::check::Check;

use crate::conformance::version_and_hashes;
use crate::pack::{Pack, walk};

/// Lint `repo_dir` against `pack`. Ids: `pattern.version`, `pattern.hashes`,
/// `pattern.files`, `pattern.scripts`, `pattern.pins`, `pattern.pins.lock`,
/// `pattern.overrides`, `pattern.majors`, `pattern.forbidden`,
/// `pattern.lockfile`, `pattern.wrangler.*`, `pattern.placeholders`,
/// `pattern.legacy`.
pub fn lint_repo(pack: &Pack, repo_dir: &Path) -> Vec<Check> {
    let m = &pack.manifest;
    let mut out = version_and_hashes(pack, repo_dir);
    out.push(required_files(&m.required.files, repo_dir));

    let pkg = read_json(&repo_dir.join("package.json"));
    let lock = read_json(&repo_dir.join("package-lock.json"));
    match &pkg {
        Ok(pkg) => {
            out.push(scripts(&m.required.scripts, pkg));
            out.push(pins(pack, pkg));
            out.push(overrides(&m.overrides, pkg));
            out.push(forbidden(&m.forbidden.dependencies, pkg));
        }
        Err(e) => {
            for id in [
                "pattern.scripts",
                "pattern.pins",
                "pattern.overrides",
                "pattern.forbidden",
            ] {
                out.push(
                    Check::fail(id, "package.json is missing or unreadable")
                        .with_evidence(e.clone()),
                );
            }
        }
    }
    match &lock {
        Ok(lock) => {
            out.push(locked_pins(pack, lock));
            out.push(lockfile_counts(&m.lockfile.required_prefix_counts, lock));
        }
        Err(e) => {
            for id in ["pattern.pins.lock", "pattern.lockfile"] {
                out.push(
                    Check::fail(id, "package-lock.json is missing or unreadable")
                        .with_evidence(e.clone()),
                );
            }
        }
    }
    out.push(majors(&m.majors, pkg.as_ref().ok(), lock.as_ref().ok()));
    out.extend(wrangler(pack, repo_dir));
    out.push(placeholders(pack, repo_dir));
    out.push(legacy(&m.forbidden.legacy_markers, repo_dir));
    out
}

fn read_json(path: &Path) -> Result<Json, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn summarize(id: &str, ok: &str, problems: Vec<String>, fail: bool) -> Check {
    if problems.is_empty() {
        Check::pass(id, ok)
    } else {
        let summary = if problems.len() == 1 {
            problems[0].clone()
        } else {
            format!("{} problems: {}", problems.len(), problems[0])
        };
        let c = if fail {
            Check::fail(id, summary)
        } else {
            Check::warn(id, summary)
        };
        c.with_evidence(problems.join("\n"))
    }
}

fn required_files(files: &[String], repo_dir: &Path) -> Check {
    let missing: Vec<String> = files
        .iter()
        .filter(|f| !repo_dir.join(f).is_file())
        .map(|f| format!("{f} is missing"))
        .collect();
    summarize(
        "pattern.files",
        &format!("all {} required files present", files.len()),
        missing,
        true,
    )
}

fn scripts(required: &BTreeMap<String, String>, pkg: &Json) -> Check {
    let mut problems = Vec::new();
    for (name, prefix) in required {
        match pkg.pointer(&format!(
            "/scripts/{}",
            name.replace('~', "~0").replace('/', "~1")
        )) {
            None => problems.push(format!("script `{name}` is missing")),
            Some(v) => {
                let s = v.as_str().unwrap_or_default();
                if !s.starts_with(prefix.as_str()) {
                    problems.push(format!(
                        "script `{name}` must start with `{prefix}` (is `{s}`)"
                    ));
                }
            }
        }
    }
    summarize(
        "pattern.scripts",
        "conformance gate runs first in `ci`",
        problems,
        true,
    )
}

fn dep_spec<'a>(pkg: &'a Json, section: &str, name: &str) -> Option<&'a str> {
    pkg.get(section)?.get(name)?.as_str()
}

fn pins(pack: &Pack, pkg: &Json) -> Check {
    let m = &pack.manifest;
    let mut problems = Vec::new();
    for (section, pins) in [("dependencies", &m.pins), ("devDependencies", &m.dev_pins)] {
        for (name, want) in pins {
            match dep_spec(pkg, section, name) {
                None => problems.push(format!("{name} is not in {section} (want {want})")),
                Some(have) if have != want => problems.push(format!(
                    "{name} is `{have}` in {section}, want exactly `{want}`"
                )),
                Some(_) => {}
            }
        }
    }
    summarize(
        "pattern.pins",
        &format!("{} exact pins match", m.pins.len() + m.dev_pins.len()),
        problems,
        true,
    )
}

fn locked_version<'a>(lock: &'a Json, name: &str) -> Option<&'a str> {
    lock.get("packages")?
        .get(format!("node_modules/{name}"))?
        .get("version")?
        .as_str()
}

fn locked_pins(pack: &Pack, lock: &Json) -> Check {
    let m = &pack.manifest;
    let mut problems = Vec::new();
    for (name, want) in m.pins.iter().chain(&m.dev_pins).chain(&m.overrides) {
        match locked_version(lock, name) {
            None => problems.push(format!("{name} is not resolved in package-lock.json")),
            Some(have) if have != want => problems.push(format!(
                "{name} resolves to {have} in package-lock.json, want {want}"
            )),
            Some(_) => {}
        }
    }
    summarize(
        "pattern.pins.lock",
        "pins and overrides resolve exactly in package-lock.json",
        problems,
        true,
    )
}

fn overrides(want: &BTreeMap<String, String>, pkg: &Json) -> Check {
    let mut problems = Vec::new();
    for (name, v) in want {
        match dep_spec(pkg, "overrides", name) {
            None => problems.push(format!("overrides.{name} is missing (want {v})")),
            Some(have) if have != v => {
                problems.push(format!("overrides.{name} is `{have}`, want `{v}`"))
            }
            Some(_) => {}
        }
    }
    summarize(
        "pattern.overrides",
        &format!("{} overrides match", want.len()),
        problems,
        true,
    )
}

fn forbidden(names: &[String], pkg: &Json) -> Check {
    let mut problems = Vec::new();
    for section in ["dependencies", "devDependencies"] {
        for name in names {
            if dep_spec(pkg, section, name).is_some() {
                problems.push(format!("{name} is in {section}"));
            }
        }
    }
    summarize(
        "pattern.forbidden",
        "no forbidden dependencies",
        problems,
        true,
    )
}

/// The leading major number of a version or simple range (`^4.6.5` → 4).
fn major_of(spec: &str) -> Option<u64> {
    let s = spec.trim_start_matches(['^', '~', '>', '=', 'v', ' ']);
    s.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

fn majors(floors: &BTreeMap<String, u64>, pkg: Option<&Json>, lock: Option<&Json>) -> Check {
    let mut problems = Vec::new();
    for (name, floor) in floors {
        let spec = pkg.and_then(|p| {
            dep_spec(p, "dependencies", name).or_else(|| dep_spec(p, "devDependencies", name))
        });
        match spec {
            None => problems.push(format!(
                "{name} is not a dependency (want major >= {floor})"
            )),
            Some(s) => {
                if major_of(s).is_some_and(|m| m < *floor) {
                    problems.push(format!("{name} `{s}` is below major {floor}"));
                }
            }
        }
        if let Some(v) = lock
            .and_then(|l| locked_version(l, name))
            .filter(|v| major_of(v).is_some_and(|m| m < *floor))
        {
            problems.push(format!("{name} resolves to {v}, below major {floor}"));
        }
    }
    summarize(
        "pattern.majors",
        &format!(
            "{} ranged dependencies at or above their major floor",
            floors.len()
        ),
        problems,
        true,
    )
}

fn lockfile_counts(want: &BTreeMap<String, usize>, lock: &Json) -> Check {
    let mut problems = Vec::new();
    let packages = lock.get("packages").and_then(|p| p.as_object());
    let mut found = Vec::new();
    for (prefix, n) in want {
        let key = format!("node_modules/{prefix}");
        let entries: Vec<(&String, &Json)> = packages
            .map(|p| p.iter().filter(|(k, _)| k.starts_with(&key)).collect())
            .unwrap_or_default();
        if entries.len() < *n {
            problems.push(format!("{} `{prefix}*` entries, want {n}", entries.len()));
        }
        for (k, v) in &entries {
            if v.get("version")
                .and_then(|v| v.as_str())
                .is_none_or(str::is_empty)
            {
                problems.push(format!("{k} has no version"));
            }
        }
        found.push(format!("{} `{prefix}*`", entries.len()));
    }
    summarize(
        "pattern.lockfile",
        &format!("lockfile carries {}", found.join(", ")),
        problems,
        true,
    )
}

fn host_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next()?;
    Some(host.rsplit_once('@').map_or(host, |(_, h)| h)).filter(|h| !h.is_empty())
}

fn wrangler(pack: &Pack, repo_dir: &Path) -> Vec<Check> {
    let rules = &pack.manifest.wrangler;
    let path = repo_dir.join("wrangler.toml");
    let table: toml::Table = match std::fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|t| toml::from_str(&t).map_err(|e| e.to_string()))
    {
        Ok(t) => t,
        Err(e) => {
            return vec![
                Check::fail(
                    "pattern.wrangler",
                    "wrangler.toml is missing or unparseable",
                )
                .with_evidence(e),
            ];
        }
    };
    let s = |k: &str| table.get(k).and_then(|v| v.as_str());
    let vars = table.get("vars").and_then(|v| v.as_table());
    let var = |k: &str| vars.and_then(|v| v.get(k)).and_then(|v| v.as_str());
    let mut out = Vec::new();

    if let Some(main) = &rules.main {
        out.push(match s("main") {
            Some(m) if m == main => Check::pass("pattern.wrangler.main", format!("main = {m}")),
            other => Check::fail(
                "pattern.wrangler.main",
                format!("main is {}, want {main}", other.unwrap_or("unset")),
            ),
        });
    }

    let flags: BTreeSet<&str> = table
        .get("compatibility_flags")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let missing: Vec<String> = rules
        .compatibility_flags
        .iter()
        .filter(|f| !flags.contains(f.as_str()))
        .map(|f| format!("compatibility flag {f} is missing"))
        .collect();
    out.push(summarize(
        "pattern.wrangler.flags",
        &format!(
            "compatibility flags include {}",
            rules.compatibility_flags.join(", ")
        ),
        missing,
        true,
    ));

    if let Some(min) = &rules.min_compatibility_date {
        out.push(match s("compatibility_date") {
            Some(d) if d >= min.as_str() => {
                Check::pass("pattern.wrangler.date", format!("compatibility_date {d}"))
            }
            Some(d) => Check::fail(
                "pattern.wrangler.date",
                format!("compatibility_date {d} is older than {min}"),
            ),
            None => Check::fail("pattern.wrangler.date", "compatibility_date is unset"),
        });
    }

    let kv: BTreeSet<&str> = table
        .get("kv_namespaces")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|t| t.get("binding").and_then(|b| b.as_str()))
                .collect()
        })
        .unwrap_or_default();
    out.push(summarize(
        "pattern.wrangler.kv",
        &format!(
            "KV bindings include {}",
            rules.required_kv_bindings.join(", ")
        ),
        rules
            .required_kv_bindings
            .iter()
            .filter(|b| !kv.contains(b.as_str()))
            .map(|b| format!("KV binding {b} is missing"))
            .collect(),
        true,
    ));

    out.push(summarize(
        "pattern.wrangler.vars",
        &format!("all {} required vars set", rules.required_vars.len()),
        rules
            .required_vars
            .iter()
            .filter(|v| var(v).is_none_or(str::is_empty))
            .map(|v| format!("[vars] {v} is missing"))
            .collect(),
        true,
    ));

    if let Some(pv) = &rules.public_url_var {
        let mut hosts = Vec::new();
        let mut collect = |v: &toml::Value| match v {
            toml::Value::String(p) => hosts.push(p.clone()),
            toml::Value::Table(t) => {
                if let Some(p) = t.get("pattern").and_then(|p| p.as_str()) {
                    hosts.push(p.to_string());
                }
            }
            _ => {}
        };
        if let Some(r) = table.get("route") {
            collect(r);
        }
        if let Some(rs) = table.get("routes").and_then(|r| r.as_array()) {
            rs.iter().for_each(&mut collect);
        }
        let route_hosts: Vec<String> = hosts
            .iter()
            .filter_map(|p| host_of(p).map(|h| h.trim_end_matches("/*").to_string()))
            .collect();
        let public = var(pv).and_then(host_of);
        out.push(match (public, route_hosts.is_empty()) {
            (None, _) => Check::fail(
                "pattern.wrangler.route",
                format!("[vars] {pv} is not a URL"),
            ),
            (Some(h), true) => Check::pass(
                "pattern.wrangler.route",
                format!("no custom-domain route; {pv} host is {h}"),
            ),
            (Some(h), false) if route_hosts.iter().any(|r| r == h) => Check::pass(
                "pattern.wrangler.route",
                format!("route host matches {pv} ({h})"),
            ),
            (Some(h), false) => Check::fail(
                "pattern.wrangler.route",
                format!("route host(s) {} != {pv} host {h}", route_hosts.join(", ")),
            ),
        });
    }

    if rules.forbid_durable_objects {
        out.push(durable_objects(&table));
    }

    if let Some(sv) = &rules.scope_var {
        out.push(scopes(sv, var(sv)));
    }
    out
}

fn durable_objects(table: &toml::Table) -> Check {
    let id = "pattern.wrangler.durable_objects";
    let bindings: Vec<String> = table
        .get("durable_objects")
        .and_then(|d| d.get("bindings"))
        .and_then(|b| b.as_array())
        .map(|a| {
            a.iter()
                .map(|b| {
                    let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let class = b.get("class_name").and_then(|v| v.as_str()).unwrap_or("?");
                    format!("{name} → {class}")
                })
                .collect()
        })
        .unwrap_or_default();
    let mut live: BTreeSet<String> = BTreeSet::new();
    let mut retired: BTreeSet<String> = BTreeSet::new();
    let strs = |m: &toml::Value, k: &str| -> Vec<String> {
        m.get(k)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    for m in table
        .get("migrations")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        for c in strs(m, "new_classes")
            .into_iter()
            .chain(strs(m, "new_sqlite_classes"))
        {
            retired.remove(&c);
            live.insert(c);
        }
        if let Some(rs) = m.get("renamed_classes").and_then(|v| v.as_array()) {
            for r in rs {
                if let (Some(from), Some(to)) = (
                    r.get("from").and_then(|v| v.as_str()),
                    r.get("to").and_then(|v| v.as_str()),
                ) {
                    live.remove(from);
                    live.insert(to.to_string());
                }
            }
        }
        for c in strs(m, "deleted_classes") {
            live.remove(&c);
            retired.insert(c);
        }
    }
    if !bindings.is_empty() || !live.is_empty() {
        let mut ev = Vec::new();
        if !bindings.is_empty() {
            ev.push(format!("durable_objects.bindings: {}", bindings.join(", ")));
        }
        if !live.is_empty() {
            ev.push(format!(
                "migrated classes never deleted: {}",
                live.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
        return Check::fail(
            id,
            "a live Durable Object binding: the MCP layer must be stateless",
        )
        .with_evidence(ev.join("\n"));
    }
    if retired.is_empty() {
        Check::pass(id, "no Durable Objects")
    } else {
        Check::pass(
            id,
            format!(
                "no live Durable Objects (retired in migration history: {})",
                retired.into_iter().collect::<Vec<_>>().join(", ")
            ),
        )
    }
}

fn scopes(var: &str, value: Option<&str>) -> Check {
    let id = "pattern.wrangler.scopes";
    let Some(v) = value.filter(|v| !v.trim().is_empty()) else {
        return Check::fail(id, format!("[vars] {var} is unset"));
    };
    let shape = Regex::new(r"^[a-z0-9][a-z0-9_-]*:[a-z0-9][a-z0-9_-]*$").expect("static regex");
    let list: Vec<&str> = v.split_whitespace().collect();
    let mut problems = Vec::new();
    for s in &list {
        if !shape.is_match(s) {
            problems.push(format!("`{s}` is not `<slug>:<action>`"));
        }
    }
    let slugs: BTreeSet<&str> = list
        .iter()
        .filter_map(|s| s.split_once(':').map(|p| p.0))
        .collect();
    if slugs.len() > 1 {
        problems.push(format!(
            "scopes span several slugs: {}",
            slugs.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    summarize(
        id,
        &format!("per-server scopes: {}", list.join(" ")),
        problems,
        false,
    )
}

fn placeholders(pack: &Pack, repo_dir: &Path) -> Check {
    let id = "pattern.placeholders";
    let p = &pack.manifest.placeholders;
    if p.patterns.is_empty() {
        return Check::skip(id, "the pack declares no placeholder patterns");
    }
    let joined = p
        .patterns
        .iter()
        .map(|s| format!("(?:{s})"))
        .collect::<Vec<_>>()
        .join("|");
    let (re, exclude) = match (
        Regex::new(&joined),
        p.exclude.as_deref().map(Regex::new).transpose(),
    ) {
        (Ok(r), Ok(e)) => (r, e),
        (Err(e), _) | (_, Err(e)) => {
            return Check::fail(id, "pattern.toml has an invalid placeholder regex")
                .with_evidence(e.to_string());
        }
    };
    let files = match walk(repo_dir) {
        Ok(f) => f,
        Err(e) => return Check::fail(id, "could not list the repo").with_evidence(e.to_string()),
    };
    let mut hits = Vec::new();
    for (rel, path) in files {
        let ext = rel.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        if !p.extensions.is_empty() && !p.extensions.iter().any(|x| x == ext) {
            continue;
        }
        if exclude.as_ref().is_some_and(|x| x.is_match(&rel)) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (n, line) in text.lines().enumerate() {
            if re.is_match(line) {
                hits.push(format!("{rel}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    if hits.is_empty() {
        Check::pass(id, "no template placeholders remain")
    } else {
        Check::fail(
            id,
            format!("{} unreplaced template placeholder(s)", hits.len()),
        )
        .with_evidence(hits.join("\n"))
    }
}

fn legacy(markers: &[String], repo_dir: &Path) -> Check {
    let id = "pattern.legacy";
    let mut hits = Vec::new();
    if let Ok(entries) = std::fs::read_dir(repo_dir.join("src")) {
        let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if path.extension().and_then(|e| e.to_str()) != Some("ts") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            for m in markers {
                if text.contains(m.as_str()) {
                    hits.push(format!("src/{name} mentions {m}"));
                }
            }
        }
    }
    summarize(
        id,
        "no legacy (sessionful McpAgent) markers in src/",
        hits,
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn majors_parse_ranges() {
        assert_eq!(major_of("^4.6.5"), Some(4));
        assert_eq!(major_of("5.20261001.0-alpha"), Some(5));
        assert_eq!(major_of(">=22"), Some(22));
        assert_eq!(major_of("latest"), None);
    }

    #[test]
    fn hosts_come_out_of_urls_and_patterns() {
        assert_eq!(
            host_of("https://x.mcp.acme.example/mcp"),
            Some("x.mcp.acme.example")
        );
        assert_eq!(host_of("x.mcp.acme.example"), Some("x.mcp.acme.example"));
        assert_eq!(host_of("x.acme.example/*"), Some("x.acme.example"));
    }

    #[test]
    fn scope_shapes() {
        assert_eq!(
            scopes("S", Some("weather:read weather:write")).status,
            studio_core::check::Status::Pass
        );
        assert_eq!(
            scopes("S", Some("weather:read other:read")).status,
            studio_core::check::Status::Warn
        );
        assert_eq!(
            scopes("S", Some("mcp-access")).status,
            studio_core::check::Status::Warn
        );
        assert_eq!(scopes("S", None).status, studio_core::check::Status::Fail);
    }
}
