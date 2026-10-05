//! A tiny programmable HTTP mock: `METHOD /path` → canned response.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, Response};

#[derive(Clone)]
pub struct Canned {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

pub fn json(status: u16, v: serde_json::Value) -> Canned {
    Canned {
        status,
        headers: vec![("content-type".into(), "application/json".into())],
        body: v.to_string(),
    }
}

/// `(METHOD /path, Authorization header)` per request.
pub type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;

pub struct Mock {
    pub base: String,
    routes: Arc<Mutex<HashMap<String, Canned>>>,
    pub seen: Seen,
}

impl Mock {
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes: Arc<Mutex<HashMap<String, Canned>>> = Arc::default();
        let seen: Seen = Arc::default();
        let (r, s) = (routes.clone(), seen.clone());
        let app = axum::Router::new().fallback(move |req: Request<Body>| {
            let (r, s) = (r.clone(), s.clone());
            async move {
                let key = format!("{} {}", req.method(), req.uri().path());
                let auth = req
                    .headers()
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .map(String::from);
                s.lock().unwrap().push((key.clone(), auth));
                let c = r.lock().unwrap().get(&key).cloned();
                let c = c.unwrap_or(Canned {
                    status: 404,
                    headers: vec![],
                    body: r#"{"message":"Not Found","success":false}"#.into(),
                });
                let mut b = Response::builder().status(c.status);
                for (k, v) in &c.headers {
                    b = b.header(k, v);
                }
                b.body(Body::from(c.body)).unwrap()
            }
        });
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { base, routes, seen }
    }

    pub fn on(&self, method_path: &str, c: Canned) -> &Self {
        self.routes
            .lock()
            .unwrap()
            .insert(method_path.to_string(), c);
        self
    }

    pub fn hits(&self, method_path: &str) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| k == method_path)
            .count()
    }
}

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

pub fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), dst).unwrap();
        }
    }
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Acme Test")
        .env("GIT_AUTHOR_EMAIL", "test@acme.example")
        .env("GIT_COMMITTER_NAME", "Acme Test")
        .env("GIT_COMMITTER_EMAIL", "test@acme.example")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
