//! `[server] lan_only` through the real router: who gets in, who does not, and
//! what the refusal looks like on the wire.
//!
//! The tests cannot dial in from `203.0.113.5`, so the peer address is
//! overwritten in the request extensions by `testing::spawn_server_from` —
//! outside the router, and therefore outside the gate. Everything the gate
//! then does (canonicalising the peer, reading `X-Forwarded-For` only from a
//! trusted proxy, walking the chain right to left) is the wiring the binary
//! runs, on the routes the binary serves.

use std::net::SocketAddr;
use std::sync::Arc;

use serde_json::Value;
use wingspan_server::config::Config;
use wingspan_server::state::AppState;
use wingspan_server::testing::{self, ServerHandle};

/// A LAN-gated config bound beyond loopback, fronted by `trusted`.
fn lan_config(trusted: &[&str]) -> Config {
    let mut config = testing::demo_config(testing::scratch_dir());
    config.server.bind = "0.0.0.0:8787".into();
    config.server.allow_remote = true;
    config.server.lan_only = true;
    config.server.trusted_proxies = trusted.iter().map(ToString::to_string).collect();
    // Reached over the wire as `127.0.0.1:<port>`, which is a loopback name
    // and always ours; the host guard has its own tests below.
    config
}

async fn state_with(config: Config) -> Arc<AppState> {
    AppState::build(config, testing::demo_options())
        .await
        .expect("the fixture book builds")
}

/// A server that sees every connection as coming from `peer`.
async fn server_from(config: Config, peer: &str) -> ServerHandle {
    let addr: SocketAddr = peer.parse().expect("a peer address");
    testing::spawn_server_from(state_with(config).await, addr).await
}

/// (status, content-type, body-or-null) — a refusal has to be problem+json.
async fn send(request: reqwest::RequestBuilder) -> (reqwest::StatusCode, String, Value) {
    let response = request.send().await.expect("request");
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let text = response.text().await.expect("body");
    let value = serde_json::from_str(&text).unwrap_or(Value::Null);
    (status, content_type, value)
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

#[tokio::test]
async fn a_lan_client_with_no_proxy_in_front_is_served() {
    let handle = server_from(lan_config(&[]), "192.168.1.20:51000").await;
    let (status, _, body) = send(client().get(handle.api("/health"))).await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["status"], "ok");
}

/// A dual-stack listener reports every IPv4 connection as `::ffff:a.b.c.d`.
#[tokio::test]
async fn an_ipv4_mapped_peer_is_still_a_lan_peer() {
    let handle = server_from(lan_config(&[]), "[::ffff:192.168.1.20]:51000").await;
    let (status, _, body) = send(client().get(handle.api("/health"))).await;
    assert!(status.is_success(), "{body}");
}

#[tokio::test]
async fn a_client_off_the_lan_is_refused_in_problem_json() {
    let handle = server_from(lan_config(&[]), "203.0.113.5:51000").await;
    let (status, content_type, problem) = send(client().get(handle.api("/health"))).await;
    assert_eq!(status, 403, "{problem}");
    assert_eq!(content_type, "application/problem+json");
    assert!(
        problem["type"].as_str().unwrap().contains("outside-lan"),
        "{problem}"
    );
    assert!(
        problem["detail"].as_str().unwrap().contains("203.0.113.5"),
        "{problem}"
    );
}

/// The whole reason `X-Forwarded-For` is only read from a named proxy.
#[tokio::test]
async fn a_spoofed_forwarded_for_buys_nothing_without_a_proxy() {
    let handle = server_from(lan_config(&[]), "203.0.113.5:51000").await;
    let (status, _, problem) = send(
        client()
            .get(handle.api("/health"))
            .header("x-forwarded-for", "192.168.1.20"),
    )
    .await;
    assert_eq!(status, 403, "{problem}");
    assert!(problem["type"].as_str().unwrap().contains("outside-lan"));

    // Nor with a trusted proxy configured, when the request did not come
    // through it: only the *peer* decides whether the header is read.
    let handle = server_from(lan_config(&["10.9.9.9"]), "203.0.113.5:51000").await;
    let (status, _, problem) = send(
        client()
            .get(handle.api("/health"))
            .header("x-forwarded-for", "192.168.1.20"),
    )
    .await;
    assert_eq!(status, 403, "{problem}");
}

#[tokio::test]
async fn a_trusted_proxy_names_the_client_and_is_believed() {
    let handle = server_from(lan_config(&["10.9.9.9"]), "10.9.9.9:51000").await;

    let (status, _, body) = send(
        client()
            .get(handle.api("/health"))
            .header("x-forwarded-for", "192.168.1.20"),
    )
    .await;
    assert!(status.is_success(), "{body}");

    // …and refused when the client it names is not on the LAN.
    let (status, _, problem) = send(
        client()
            .get(handle.api("/health"))
            .header("x-forwarded-for", "203.0.113.5"),
    )
    .await;
    assert_eq!(status, 403, "{problem}");
    assert!(problem["type"].as_str().unwrap().contains("outside-lan"));

    // HAProxy left on `option forwardfor` (append) rather than
    // `set-header`: the client wrote a LAN address, the proxy appended the
    // address it actually saw. The rightmost entry is the only one the proxy
    // vouches for, so the lie loses.
    let (status, _, problem) = send(
        client()
            .get(handle.api("/health"))
            .header("x-forwarded-for", "192.168.1.20, 203.0.113.5"),
    )
    .await;
    assert_eq!(status, 403, "{problem}");
    assert!(
        problem["detail"].as_str().unwrap().contains("203.0.113.5"),
        "{problem}"
    );
}

#[tokio::test]
async fn a_trusted_proxy_that_forwards_nothing_is_refused_not_trusted() {
    let handle = server_from(lan_config(&["10.9.9.9"]), "10.9.9.9:51000").await;
    let (status, content_type, problem) = send(client().get(handle.api("/health"))).await;
    assert_eq!(status, 403, "{problem}");
    assert_eq!(content_type, "application/problem+json");
    assert!(
        problem["type"]
            .as_str()
            .unwrap()
            .contains("missing-forwarded-for"),
        "{problem}"
    );
    assert!(
        problem["detail"]
            .as_str()
            .unwrap()
            .contains("X-Forwarded-For"),
        "{problem}"
    );

    // A chain with rubbish in it stops the walk rather than being stepped
    // over to reach whatever an attacker put to its left.
    let (status, _, problem) = send(
        client()
            .get(handle.api("/health"))
            .header("x-forwarded-for", "192.168.1.20, unknown"),
    )
    .await;
    assert_eq!(status, 403, "{problem}");
    assert!(
        problem["type"]
            .as_str()
            .unwrap()
            .contains("untrusted-proxy"),
        "{problem}"
    );
}

/// The gate is the outermost layer, so it covers the streaming route and the
/// SPA — not just the JSON handlers.
#[tokio::test]
async fn the_spa_and_the_event_stream_are_refused_too() {
    let handle = server_from(lan_config(&[]), "203.0.113.5:51000").await;
    for url in [
        handle.url("/"),
        handle.url("/assets/app.js"),
        handle.api("/events"),
    ] {
        let (status, content_type, problem) = send(client().get(&url)).await;
        assert_eq!(status, 403, "{url}: {problem}");
        assert_eq!(content_type, "application/problem+json", "{url}");
        assert!(
            problem["type"].as_str().unwrap().contains("outside-lan"),
            "{url}: {problem}"
        );
    }

    // The same page and stream are there for a client on the LAN.
    let handle = server_from(lan_config(&[]), "192.168.1.20:51000").await;
    let page = client().get(handle.url("/")).send().await.unwrap();
    assert_ne!(page.status(), 403);
}

/// A LAN-gated remote bind is reached by a name, and the name is checked —
/// `lan_only` gates the address, and a rebound name satisfies that as easily
/// as the real one does.
#[tokio::test]
async fn the_host_guard_applies_on_a_remote_bind_too() {
    let mut config = lan_config(&[]);
    config.server.allowed_hosts = vec!["wingspan.example.com".into()];
    let handle = server_from(config, "192.168.1.20:51000").await;

    let (status, _, body) = send(
        client()
            .get(handle.api("/health"))
            .header("host", "wingspan.example.com"),
    )
    .await;
    assert!(status.is_success(), "{body}");

    let (status, _, problem) = send(
        client()
            .get(handle.api("/health"))
            .header("host", "evil.example"),
    )
    .await;
    assert_eq!(status, 403, "{problem}");
    assert!(
        problem["type"].as_str().unwrap().contains("untrusted-host"),
        "{problem}"
    );
}

/// A machine reached at its own LAN address has to say so: `bind =
/// "0.0.0.0:8787"` names every interface and therefore no host.
#[tokio::test]
async fn the_bind_address_is_ours_when_it_is_a_real_one() {
    let mut config = lan_config(&[]);
    config.server.bind = "192.168.1.50:8787".into();
    let handle = server_from(config, "192.168.1.20:51000").await;

    let (status, _, body) = send(
        client()
            .get(handle.api("/health"))
            .header("host", "192.168.1.50:8787"),
    )
    .await;
    assert!(status.is_success(), "{body}");

    let (status, _, problem) = send(
        client()
            .get(handle.api("/health"))
            .header("host", "192.168.1.51:8787"),
    )
    .await;
    assert_eq!(status, 403, "{problem}");
}

/// Nothing about `lan_only` changes when the bind is loopback: it is simply a
/// second lock, and it is honoured.
#[tokio::test]
async fn lan_only_is_honoured_on_a_loopback_bind() {
    let mut config = testing::demo_config(testing::scratch_dir());
    config.server.lan_only = true;
    config.server.lan_networks = vec!["192.168.1.0/24".into()];
    let handle = server_from(config, "127.0.0.1:51000").await;
    // 127.0.0.1 is not in the *configured* list, and an explicit list replaces
    // the built-in one.
    let (status, _, problem) = send(client().get(handle.api("/health"))).await;
    assert_eq!(status, 403, "{problem}");
    assert!(problem["type"].as_str().unwrap().contains("outside-lan"));
}
