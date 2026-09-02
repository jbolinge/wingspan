//! Plan §4: the page shows the whole book, so a bind beyond loopback is
//! refused unless the operator asked for it *and* set a gate — either a
//! bearer token (`/api` then requires it on every request) or `lan_only`
//! (every request must come from the LAN; see `tests/lan_guard.rs`).

use std::sync::Arc;

use wingspan_server::config::Config;
use wingspan_server::state::AppState;
use wingspan_server::{serve, testing};

fn remote(token: Option<&str>, allow: bool) -> Config {
    let mut config = testing::demo_config(testing::scratch_dir());
    config.server.bind = "0.0.0.0:8787".into();
    config.server.allow_remote = allow;
    config.server.auth_token = token.map(ToString::to_string);
    config
}

async fn state_with(config: Config) -> Arc<AppState> {
    AppState::build(config, testing::demo_options())
        .await
        .expect("the fixture book builds")
}

#[tokio::test]
async fn a_remote_bind_without_the_opt_in_never_reaches_the_listener() {
    let config = remote(None, false);
    let state = state_with(config.clone()).await;
    let err = serve::run(&config, state).await.unwrap_err();
    let message = format!("{err}");
    assert!(
        message.contains("refusing to bind 0.0.0.0:8787"),
        "{message}"
    );
    assert!(message.contains("allow_remote"), "{message}");
}

#[tokio::test]
async fn a_remote_bind_with_no_gate_at_all_is_refused_too() {
    let config = remote(None, true);
    let state = state_with(config.clone()).await;
    let err = serve::run(&config, state).await.unwrap_err();
    let message = format!("{err}");
    assert!(message.contains("auth_token"), "{message}");
    assert!(message.contains("lan_only"), "{message}");
}

/// The matrix the two ways out are drawn from, without opening a socket.
#[test]
fn the_gate_matrix_is_exactly_the_documented_one() {
    let mut config = Config::default();
    assert!(config.check_bind_policy().is_ok(), "loopback needs nothing");

    config.server.bind = "0.0.0.0:8787".into();
    assert!(config.check_bind_policy().is_err(), "remote, no opt-in");

    config.server.allow_remote = true;
    assert!(config.check_bind_policy().is_err(), "remote, no gate");

    let mut lan = config.clone();
    lan.server.lan_only = true;
    assert!(lan.check_bind_policy().is_ok(), "remote + lan_only");
    assert!(
        lan.required_token().is_none(),
        "lan_only must not also demand a bearer the SPA cannot send"
    );

    let mut bearer = config.clone();
    bearer.server.auth_token = Some("hunter2".into());
    assert!(bearer.check_bind_policy().is_ok(), "remote + auth_token");

    // A gate that cannot be built is not a gate, on any bind.
    let mut broken = lan.clone();
    broken.server.lan_networks = vec!["192.168.0.0/99".into()];
    let refused = broken.check_bind_policy().unwrap_err();
    assert!(refused.reason.contains("lan_networks"), "{refused}");
    assert!(refused.reason.contains("192.168.0.0/99"), "{refused}");

    let mut broken = Config::default();
    broken.server.trusted_proxies = vec!["haproxy.internal".into()];
    let refused = broken.check_bind_policy().unwrap_err();
    assert!(refused.reason.contains("trusted_proxies"), "{refused}");
}

#[tokio::test]
async fn loopback_needs_no_token_at_all() {
    let handle = testing::demo_server().await;
    let response = reqwest::get(handle.api("/health")).await.unwrap();
    assert!(response.status().is_success());
}

/// The listener is loopback (a test cannot rely on a routable address), but the
/// middleware reads the *configured* bind, so the policy is exercised exactly
/// as it would be in production.
#[tokio::test]
async fn a_remote_configuration_demands_the_bearer_on_every_api_call() {
    let state = state_with(remote(Some("hunter2"), true)).await;
    let handle = testing::spawn_server(state).await;
    let client = reqwest::Client::new();

    for path in ["/health", "/trades", "/config", "/events"] {
        let response = client.get(handle.api(path)).send().await.unwrap();
        assert_eq!(response.status(), 401, "{path} was served unauthenticated");
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .unwrap()
                .to_str()
                .unwrap(),
            "application/problem+json"
        );
        let problem: serde_json::Value = response.json().await.unwrap();
        assert!(problem["detail"].as_str().unwrap().contains("Bearer"));
    }

    let wrong = client
        .get(handle.api("/health"))
        .bearer_auth("open-sesame")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);

    let right = client
        .get(handle.api("/health"))
        .bearer_auth("hunter2")
        .send()
        .await
        .unwrap();
    assert!(right.status().is_success());
    let body: serde_json::Value = right.json().await.unwrap();
    assert_eq!(body["status"], "ok");

    // The SPA itself is not behind the token — only the data is.
    let page = client.get(handle.url("/")).send().await.unwrap();
    assert_ne!(page.status(), 401);
}

#[tokio::test]
async fn the_token_is_never_echoed_back_by_the_config_endpoint() {
    let state = state_with(remote(Some("hunter2"), true)).await;
    let handle = testing::spawn_server(state).await;
    let body: serde_json::Value = reqwest::Client::new()
        .get(handle.api("/config"))
        .bearer_auth("hunter2")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["server"]["auth_token"], "***");
    assert!(!body.to_string().contains("hunter2"));
}
