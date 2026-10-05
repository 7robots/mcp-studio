//! The login, token lifecycle and JSON-RPC client against the fake gateway.

mod common;

use std::time::Duration;

use common::Env;
use serde_json::json;
use studio_fake::Options;
use studio_gateway::GatewayError;
use studio_gateway::tokens::{TokenStore, Tokens};

#[tokio::test]
async fn login_grants_admin_and_whoami_reports_it() {
    let env = Env::new().await;
    let tokens = env.login().await.unwrap();
    assert!(
        tokens.has_scope("gateway:admin") && tokens.has_scope("mcp-access"),
        "{tokens:?}"
    );
    assert!(tokens.refresh_token.is_some());
    env.store().save(&tokens).unwrap();
    let identity = env.session().identity().await.unwrap();
    assert!(identity.admin);
    assert_eq!(identity.whoami.sub.as_deref(), Some("00ufakeadmin"));
    assert_eq!(
        identity.whoami.client_id.as_deref(),
        Some(tokens.client_id.as_str())
    );
}

#[tokio::test]
async fn login_reuses_the_cached_client_registration() {
    let env = Env::new().await;
    let first = env.login().await.unwrap();
    let second = env.login().await.unwrap();
    assert_eq!(first.client_id, second.client_id);
}

#[tokio::test]
async fn a_refused_admin_login_times_out_with_a_pointer_to_the_browser() {
    let env = Env::with(Options {
        idp_grants_admin: false,
        ..Options::default()
    })
    .await;
    let err = env.login().await.unwrap_err().to_string();
    assert!(err.contains("browser page"), "{err}");
}

#[tokio::test]
async fn scope_without_allowlisted_sub_is_view_only() {
    let env = Env::with(Options {
        admin_subs: vec![],
        ..Options::default()
    })
    .await;
    let identity = env.signed_in().await.identity().await.unwrap();
    assert!(identity.whoami.scopes.iter().any(|s| s == "gateway:admin"));
    assert!(!identity.admin);
}

#[tokio::test]
async fn reader_token_gets_the_step_up_challenge_for_admin_tools() {
    let env = Env::new().await;
    let _ = env.login().await.unwrap();
    let client_id = serde_json::from_str::<serde_json::Value>(
        &std::fs::read_to_string(env.cache().path).unwrap(),
    )
    .unwrap()[env.fake.base.as_str()]["client_id"]
        .as_str()
        .unwrap()
        .to_string();
    env.store()
        .save(&env.fake.mint("mcp-access", &client_id))
        .unwrap();
    let session = env.session();
    let identity = session.identity().await.unwrap();
    assert!(!identity.admin);
    let err = session
        .call_tool(
            "set_server_status",
            json!({"server": "notes", "status": "disabled"}),
        )
        .await
        .unwrap_err();
    match err {
        GatewayError::InsufficientScope { scope, description } => {
            assert_eq!(scope, "gateway:admin");
            assert!(description.contains("set_server_status"), "{description}");
        }
        other => panic!("expected InsufficientScope, got {other:?}"),
    }
}

#[tokio::test]
async fn expired_token_is_refreshed_before_use_and_the_rotation_stored() {
    let env = Env::new().await;
    let tokens = env.login().await.unwrap();
    let stale = Tokens {
        expires_at: 0,
        ..tokens.clone()
    };
    env.store().save(&stale).unwrap();
    env.session().whoami().await.unwrap();
    let stored = env.store().load().unwrap().unwrap();
    assert_ne!(stored.access_token, tokens.access_token);
    assert_ne!(stored.refresh_token, tokens.refresh_token);
    assert!(stored.is_fresh());
    assert!(env.fake.log().contains(&"token:refresh_token".to_string()));
}

#[tokio::test]
async fn a_401_refreshes_once_and_retries() {
    let env = Env::new().await;
    let session = env.signed_in().await;
    env.fake.expire_access_tokens();
    let list = session.list_servers(false, false).await.unwrap();
    assert_eq!(list.count, 2);
    let log = env.fake.log();
    assert_eq!(
        log[1..],
        [
            "mcp:401",
            "token:refresh_token",
            "mcp:tools/call:list_servers"
        ],
        "{log:?}"
    );
}

#[tokio::test]
async fn concurrent_calls_share_one_refresh() {
    let env = Env::new().await;
    let tokens = env.login().await.unwrap();
    env.store()
        .save(&Tokens {
            expires_at: 0,
            ..tokens
        })
        .unwrap();
    let session = env.session();
    let (a, b, c) = tokio::join!(
        session.whoami(),
        session.list_servers(true, false),
        session.list_tools()
    );
    a.unwrap();
    b.unwrap();
    c.unwrap();
    assert_eq!(
        env.fake
            .log()
            .iter()
            .filter(|l| *l == "token:refresh_token")
            .count(),
        1
    );
}

#[tokio::test]
async fn a_revoked_grant_expires_the_session_and_clears_the_store() {
    let env = Env::new().await;
    let tokens = env.login().await.unwrap();
    env.store()
        .save(&Tokens {
            expires_at: 0,
            ..tokens
        })
        .unwrap();
    env.fake.revoke_all_grants();
    let session = env.session();
    let err = session.whoami().await.unwrap_err();
    assert!(matches!(err, GatewayError::SessionExpired), "{err:?}");
    assert!(env.store().load().unwrap().is_none());
    // Later calls keep saying why, rather than "not signed in".
    let again = session.list_tools().await.unwrap_err();
    assert!(matches!(again, GatewayError::SessionExpired), "{again:?}");
}

#[tokio::test]
async fn no_tokens_means_not_logged_in() {
    let env = Env::new().await;
    let err = env.session().whoami().await.unwrap_err();
    assert!(matches!(err, GatewayError::NotLoggedIn(_)), "{err:?}");
    assert!(err.to_string().contains("mcp-studio gateway login"));
}

#[tokio::test]
async fn tool_errors_and_protocol_errors_are_distinct() {
    let env = Env::new().await;
    let session = env.signed_in().await;
    let err = session
        .call_tool("call_tool", json!({"server": "nope", "tool": "x"}))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, GatewayError::Tool(text) if text.contains("unknown server 'nope'")),
        "{err:?}"
    );
    let err = session
        .call_tool("no_such_tool", json!({}))
        .await
        .unwrap_err();
    assert!(
        matches!(err, GatewayError::Rpc { code: -32602, .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn plain_json_framing_is_handled() {
    let env = Env::with(Options {
        sse: false,
        ..Options::default()
    })
    .await;
    let list = env
        .signed_in()
        .await
        .list_servers(true, false)
        .await
        .unwrap();
    assert_eq!(list.count, 4);
    assert_eq!(
        list.servers.iter().filter(|s| s.status == "active").count(),
        2
    );
}

#[tokio::test]
async fn logout_revokes_the_refresh_token_at_the_gateway() {
    let env = Env::new().await;
    let tokens = env.login().await.unwrap();
    env.store().save(&tokens).unwrap();
    env.session().logout().await.unwrap();
    assert!(env.store().load().unwrap().is_none());
    // The revoked refresh token no longer works.
    env.store()
        .save(&Tokens {
            expires_at: 0,
            ..tokens
        })
        .unwrap();
    let err = env.session().whoami().await.unwrap_err();
    assert!(matches!(err, GatewayError::SessionExpired), "{err:?}");
}

#[tokio::test]
async fn a_second_process_adopts_the_rotation_instead_of_signing_both_out() {
    let env = Env::new().await;
    let tokens = env.login().await.unwrap();
    env.store()
        .save(&Tokens {
            expires_at: 0,
            ..tokens
        })
        .unwrap();
    // Two processes, each with its own copy of the same stale pair.
    let first = env.session();
    let second = env.session();
    first.whoami().await.unwrap();
    let rotated = env.store().load().unwrap().unwrap();
    // The second still holds the old pair; it must pick up the first's rotation.
    second.whoami().await.unwrap();
    assert_eq!(
        env.store().load().unwrap().unwrap().refresh_token,
        rotated.refresh_token
    );
    assert_eq!(
        env.fake
            .log()
            .iter()
            .filter(|l| *l == "token:refresh_token")
            .count(),
        1
    );
}

#[tokio::test]
async fn tokens_issued_by_another_gateway_are_ignored() {
    let env = Env::new().await;
    let tokens = env.login().await.unwrap();
    env.store()
        .save(&Tokens {
            gateway: Some("https://elsewhere.example/".into()),
            ..tokens
        })
        .unwrap();
    let err = env.session().whoami().await.unwrap_err();
    assert!(matches!(err, GatewayError::NotLoggedIn(_)), "{err:?}");
}

#[tokio::test]
async fn login_records_the_issuing_gateway() {
    let env = Env::new().await;
    let tokens = env.login().await.unwrap();
    assert_eq!(tokens.gateway.as_deref(), Some(env.fake.base.as_str()));
}

#[tokio::test]
async fn session_login_installs_the_pair_and_registers_under_the_profile_name() {
    let env = Env::new().await;
    let session = env.session();
    assert!(matches!(
        session.whoami().await.unwrap_err(),
        GatewayError::NotLoggedIn(_)
    ));
    let tokens = session
        .login(Duration::from_secs(2), common::browser)
        .await
        .unwrap();
    assert_eq!(env.store().load().unwrap(), Some(tokens));
    assert!(session.identity().await.unwrap().admin);
    // The registration is cached for the next sign-in.
    assert!(session.cache().get(&env.fake.base).is_some());
}

#[tokio::test]
async fn admin_needs_the_profiles_probe_tool() {
    let env = Env::new().await;
    let tokens = env.login().await.unwrap();
    env.store().save(&tokens).unwrap();
    let mut profile = env.profile();
    profile.admin_probe_tool = "a_tool_this_gateway_lacks".into();
    let session = studio_gateway::Session::new(
        studio_gateway::http_client(),
        profile,
        Box::new(env.store()),
        env.cache(),
    )
    .unwrap();
    assert!(!session.identity().await.unwrap().admin);
}

#[tokio::test]
async fn change_url_reports_its_steps_as_json() {
    let env = Env::new().await;
    let session = env.signed_in().await;
    let list = session.list_servers(true, true).await.unwrap();
    let weather = list.servers.iter().find(|s| s.id == "weather").unwrap();
    let outcome =
        studio_gateway::actions::change_url(&session, weather, "https://weather2.example.com/mcp")
            .await
            .unwrap();
    let json = outcome.to_json();
    assert_eq!(json["ok"], true);
    assert_eq!(json["result"]["now"], "https://weather2.example.com/mcp");
    // The fake re-registers with a new catalogue: policy classifications are
    // not admin records, so nothing is reported lost; status comes back.
    assert_eq!(json["result"]["not_restored"], json!([]));
    let again = session.list_servers(true, false).await.unwrap();
    let moved = again.servers.iter().find(|s| s.id == "weather").unwrap();
    assert_eq!(moved.status, "disabled");
}

fn legacy() -> Options {
    Options {
        legacy: true,
        ..Options::default()
    }
}

#[tokio::test]
async fn identity_lists_the_offered_tools_and_health_the_build() {
    let env = Env::new().await;
    let session = env.signed_in().await;
    let identity = session.identity().await.unwrap();
    assert!(identity.offers("update_server") && identity.offers("list_connections"));
    let build = session.health().await.unwrap().build.unwrap();
    assert_eq!(build.version, "1.8.0");
    assert_eq!(build.sha.as_deref(), Some("3f2a9c1e7d"));
    let list = session.list_servers(true, false).await.unwrap();
    let notes = list.servers.iter().find(|s| s.id == "notes").unwrap();
    assert_eq!(notes.server_version.as_deref(), Some("0.3.0"));
    assert_eq!(notes.registered_by.as_deref(), Some("admin@example.org"));
    assert!(notes.registered_at.is_some());

    let old = Env::with(legacy()).await;
    let session = old.signed_in().await;
    let identity = session.identity().await.unwrap();
    assert!(identity.admin && !identity.offers("update_server"));
    assert!(session.health().await.unwrap().build.is_none());
    let list = session.list_servers(true, false).await.unwrap();
    assert!(list.servers.iter().all(|s| s.server_version.is_none()));
}

#[tokio::test]
async fn scope_owners_and_connections() {
    let env = Env::new().await;
    let session = env.signed_in().await;
    let owners = session.list_scope_owners().await.unwrap();
    assert!(
        owners
            .iter()
            .any(|o| o.scope == "tasks:all" && o.server == "tasks")
    );
    assert_eq!(session.list_connections(None).await.unwrap().len(), 3);
    let keys = session.list_connections(Some("api_key")).await.unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].subject, "tasks");

    let old = Env::with(legacy()).await;
    let session = old.signed_in().await;
    let err = session.list_scope_owners().await.unwrap_err();
    assert!(
        matches!(err, GatewayError::Unsupported(ref t) if t == "list_scope_owners"),
        "{err:?}"
    );
    assert_eq!(
        session
            .list_connections(None)
            .await
            .unwrap_err()
            .to_string(),
        "list_connections is not supported by this gateway"
    );
    assert_eq!(
        old.fake
            .log()
            .iter()
            .filter(|l| l.starts_with("mcp:tools/call:list_"))
            .filter(|l| !l.ends_with("list_servers"))
            .count(),
        0,
        "never called a tool the gateway does not offer"
    );
}

#[tokio::test]
async fn change_url_uses_update_server_when_offered_and_falls_back_otherwise() {
    use studio_gateway::actions::{Action, run};
    let env = Env::new().await;
    let session = env.signed_in().await;
    let notes = session
        .list_servers(true, true)
        .await
        .unwrap()
        .servers
        .into_iter()
        .find(|s| s.id == "notes")
        .unwrap();
    let outcome = run(
        &session,
        Action::ChangeUrl {
            server: Box::new(notes.clone()),
            url: "https://notes2.mcp.example.org/mcp".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        outcome.raw["changed"]["url"]["was"],
        "https://notes.mcp.example.org/mcp"
    );
    assert_eq!(outcome.drift, vec![("notes".to_string(), None)]);
    let log = env.fake.log();
    assert!(!log.iter().any(|l| l.ends_with(":unregister_server")));
    // update_server kept the admin classifications.
    let after = session.list_servers(true, true).await.unwrap();
    let moved = after.servers.iter().find(|s| s.id == "notes").unwrap();
    assert_eq!(moved.tool_classes, notes.tool_classes);
    assert!(moved.read_only());

    // A failed probe changes nothing.
    let err = run(
        &session,
        Action::Update {
            server: "notes".into(),
            url: Some("https://fail.example.org/mcp".into()),
            display_name: Some("Renamed".into()),
            description: None,
        },
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("probe failed"), "{err}");
    let after = session.list_servers(true, false).await.unwrap();
    let same = after.servers.iter().find(|s| s.id == "notes").unwrap();
    assert_eq!(
        (same.url.as_str(), same.name.as_str()),
        ("https://notes2.mcp.example.org/mcp", "Notes")
    );

    let old = Env::with(legacy()).await;
    let session = old.signed_in().await;
    let weather = session
        .list_servers(true, true)
        .await
        .unwrap()
        .servers
        .into_iter()
        .find(|s| s.id == "weather")
        .unwrap();
    let outcome = run(
        &session,
        Action::ChangeUrl {
            server: Box::new(weather),
            url: "https://weather2.example.com/mcp".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(outcome.raw["now"], "https://weather2.example.com/mcp");
    assert!(
        old.fake
            .log()
            .iter()
            .any(|l| l.ends_with(":unregister_server"))
    );
    let err = run(
        &session,
        Action::UpdateDetails {
            server: "weather".into(),
            display_name: Some("W".into()),
            description: None,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, GatewayError::Unsupported(_)), "{err:?}");
}

#[tokio::test]
async fn revoke_user_and_call_downstream() {
    use studio_gateway::actions::{Action, run};
    let env = Env::new().await;
    let session = env.signed_in().await;
    let outcome = run(
        &session,
        Action::RevokeUser {
            sub: studio_fake::gateway::OTHER_SUB.into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(outcome.raw["grants_revoked"], 2);
    assert!(
        outcome.message.contains("user connection removed"),
        "{}",
        outcome.message
    );
    let again = run(
        &session,
        Action::RevokeUser {
            sub: studio_fake::gateway::OTHER_SUB.into(),
        },
    )
    .await
    .unwrap();
    assert!(
        again
            .message
            .contains("0 grants revoked, 0 clients, 0 login tokens removed, no user connection"),
        "{}",
        again.message
    );

    let r = session
        .call_downstream("notes", "notes_read", json!({"id": 7}))
        .await
        .unwrap();
    let text = studio_gateway::model::render_result(&r);
    assert!(text.contains("\"id\": 7"), "{text}");
    let err = session
        .call_downstream("notes", "notes_create", json!({}))
        .await
        .unwrap_err();
    assert!(
        matches!(err, GatewayError::Tool(ref m) if m.contains("read-only")),
        "{err:?}"
    );
}
