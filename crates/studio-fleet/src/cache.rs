//! `<cache_dir>/fleet.json`: the last full run, reused for `probes.ttl_seconds`.
//!
//! Only complete runs (every server, every enabled source) are stored; a
//! filtered request is answered from a fresh complete run when one covers it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use studio_core::Instance;

use crate::{FleetReport, Source, select};

#[derive(Serialize, Deserialize)]
struct Cached {
    /// Which pluggable providers were wired for this run.
    providers: Vec<String>,
    fetched: bool,
    report: FleetReport,
}

pub fn path(inst: &Instance) -> PathBuf {
    inst.cache_dir().join("fleet.json")
}

pub fn load(
    inst: &Instance,
    sources: &[Source],
    servers: &[String],
    providers: &[String],
    fetch: bool,
) -> Option<FleetReport> {
    load_from(
        &path(inst),
        inst.config.probes.ttl_seconds,
        sources,
        servers,
        providers,
        fetch,
    )
}

pub fn load_from(
    file: &Path,
    ttl_seconds: u64,
    sources: &[Source],
    servers: &[String],
    providers: &[String],
    fetch: bool,
) -> Option<FleetReport> {
    let text = std::fs::read_to_string(file).ok()?;
    let c: Cached = serde_json::from_str(&text).ok()?;
    let age = crate::time::now_unix() - c.report.generated_at_unix;
    if age < 0 || age as u64 >= ttl_seconds || c.providers != providers {
        return None;
    }
    // a --fetch request wants fresh ahead/behind; only a fetched run will do
    if fetch && !c.fetched {
        return None;
    }
    if !sources.iter().all(|s| c.report.sources.contains(s)) {
        return None;
    }
    let mut r = c.report;
    let infos: Vec<_> = r.servers.iter().map(|s| s.info.clone()).collect();
    if !servers.is_empty() {
        let keep: Vec<String> = select(&infos, servers)
            .iter()
            .map(|s| s.repo.clone())
            .collect();
        if keep.len() < servers.len() {
            return None;
        }
        r.servers.retain(|s| keep.contains(&s.repo));
    }
    if r.sources.as_slice() != sources {
        r.sources.retain(|s| sources.contains(s));
        for s in &mut r.servers {
            let cols: Vec<_> = s
                .columns
                .iter()
                .filter(|c| sources.contains(&c.source))
                .cloned()
                .collect();
            s.checks
                .retain(|c| check_source(&c.id).is_none_or(|src| sources.contains(&src)));
            s.columns = cols;
        }
    }
    r.from_cache = true;
    Some(r)
}

pub fn store(inst: &Instance, report: &FleetReport, providers: &[String], fetched: bool) {
    store_to(&path(inst), report, providers, fetched);
}

pub fn store_to(p: &Path, report: &FleetReport, providers: &[String], fetched: bool) {
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let c = Cached {
        providers: providers.to_vec(),
        fetched,
        report: report.clone(),
    };
    if let Ok(text) = serde_json::to_string(&c) {
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, p);
        }
    }
}

/// The source a check id belongs to, from its first segment.
pub fn check_source(id: &str) -> Option<Source> {
    match id.split('.').next()? {
        "git" => Some(Source::Git),
        "github" => Some(Source::Github),
        "cf" => Some(Source::Cloudflare),
        "http" => Some(Source::Http),
        "okta" => Some(Source::Okta),
        "gateway" => Some(Source::Gateway),
        "marketplace" => Some(Source::Marketplace),
        "pattern" => Some(Source::Pattern),
        _ => None,
    }
}
