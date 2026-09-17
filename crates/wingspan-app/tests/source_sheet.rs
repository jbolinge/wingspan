//! [`wingspan_app::source::sheet::GoogleSheetSource`] against a real
//! `reqwest` client talking to a localhost `wiremock` server — the same
//! harness `crates/wingspan-marketdata/tests/tastytrade_api.rs` uses for the
//! tastytrade client.
//!
//! `classify` on its own is a pure-function unit test living next to the
//! code; every case here instead goes through [`GoogleSheetSource::fetch_csv`]
//! or `::load`, because what matters is that the real HTTP stack — redirects,
//! headers, timeouts, body limits — feeds `classify` what it expects.
//!
//! The placeholder spreadsheet id below is the only one this repository ever
//! writes; `scripts/check_redaction.py` fails the build on anything else
//! that looks like a real one.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::NaiveDate;
use wingspan_app::source::sheet::GoogleSheetSource;
use wingspan_app::source::{SourceKind, SourceRef, TradeSource};
use wingspan_core::book::LoadOptions;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The one spreadsheet id this repository is ever allowed to contain.
const ID: &str = "1AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcd";

fn sheet_ref() -> SourceRef {
    SourceRef::Sheet {
        id: ID.to_string(),
        tab: "Legs".to_string(),
        gid: None,
        published: false,
    }
}

fn sheet_ref_with_gid(gid: u64) -> SourceRef {
    SourceRef::Sheet {
        id: ID.to_string(),
        tab: "Legs".to_string(),
        gid: Some(gid),
        published: false,
    }
}

fn source(server: &MockServer) -> GoogleSheetSource {
    GoogleSheetSource::new(&sheet_ref(), Duration::from_secs(5))
        .unwrap()
        .with_base_url(server.uri())
}

fn sample_csv() -> String {
    std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/sheets/legs-sample.csv"),
    )
    .expect("fixtures/sheets/legs-sample.csv is checked in")
}

/// The pinned valuation date `fixtures/sheets/legs-sample.csv`'s numbers in
/// `docs/DOMAIN.md` and `crates/wingspan-core/tests/sheet_parser.rs` are
/// asserted against.
fn options() -> LoadOptions {
    LoadOptions {
        valuation_date: Some(NaiveDate::from_ymd_opt(2026, 8, 31).unwrap()),
        ..LoadOptions::default()
    }
}

fn gviz_error_body() -> &'static str {
    "/*O_o*/\ngoogle.visualization.Query.setResponse({\"version\":\"0.6\",\"status\":\"error\",\
     \"errors\":[{\"reason\":\"invalid_query\",\"message\":\"INVALID_QUERY\",\"detailed_message\":\
     \"Invalid sheet name\"}]});"
}

#[tokio::test]
async fn the_happy_path_loads_a_book() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/spreadsheets/d/{ID}/gviz/tq")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/csv")
                .set_body_string(sample_csv()),
        )
        .mount(&server)
        .await;

    let load = source(&server)
        .load(&options())
        .await
        .expect("a shared sheet serving a valid Legs CSV loads");

    let summary = load.summary.expect("a sheet load always has a summary");
    assert_eq!(summary.rows, 11);
    assert_eq!(summary.legs, 10);
    assert_eq!(summary.rows_skipped, 1);
    assert_eq!(
        summary.columns_ignored,
        vec!["Days".to_string(), "Band".to_string()]
    );
    assert_eq!(summary.header_row, 3);
    assert!(!load.from_cache);
    assert!(!load.book.trades.is_empty());
}

#[tokio::test]
async fn a_login_redirect_is_reported_as_not_shared() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/spreadsheets/d/{ID}/gviz/tq")))
        .respond_with(
            ResponseTemplate::new(302).insert_header("Location", "/accounts/ServiceLogin"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/accounts/ServiceLogin"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/html")
                .set_body_string("<html>sign in to continue</html>"),
        )
        .mount(&server)
        .await;

    let err = source(&server)
        .fetch_csv()
        .await
        .expect_err("a redirect to sign-in is not a shared sheet");
    assert_eq!(err.slug(), "sheet-not-shared");
    assert!(err.detail().contains("Anyone with the link"), "{err:?}");
}

#[tokio::test]
async fn a_401_is_reported_as_not_shared() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/spreadsheets/d/{ID}/gviz/tq")))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let err = source(&server)
        .fetch_csv()
        .await
        .expect_err("a 401 is not a shared sheet");
    assert_eq!(err.slug(), "sheet-not-shared");
}

#[tokio::test]
async fn gvizs_error_payload_is_reported_as_tab_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/spreadsheets/d/{ID}/gviz/tq")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/javascript")
                .set_body_string(gviz_error_body()),
        )
        .mount(&server)
        .await;

    let err = source(&server)
        .fetch_csv()
        .await
        .expect_err("gviz's own error payload is a tab-not-found, not a CSV");
    assert_eq!(err.slug(), "sheet-tab-not-found");
    assert!(err.detail().contains("Legs"), "{err:?}");
}

#[tokio::test]
async fn a_tab_not_found_retries_once_with_gid_and_can_succeed() {
    let server = MockServer::start().await;
    let gid = 123_456_789u64;

    Mock::given(method("GET"))
        .and(path(format!("/spreadsheets/d/{ID}/gviz/tq")))
        .and(query_param("sheet", "Legs"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/javascript")
                .set_body_string(gviz_error_body()),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/spreadsheets/d/{ID}/gviz/tq")))
        .and(query_param("gid", gid.to_string()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/csv")
                .set_body_string(sample_csv()),
        )
        .mount(&server)
        .await;

    let reference = sheet_ref_with_gid(gid);
    let with_gid = GoogleSheetSource::new(&reference, Duration::from_secs(5))
        .unwrap()
        .with_base_url(server.uri());

    let text = with_gid
        .fetch_csv()
        .await
        .expect("the gid= retry recovers a renamed-but-still-there tab");
    assert_eq!(text, sample_csv());
}

#[tokio::test]
async fn a_500_is_unreachable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/spreadsheets/d/{ID}/gviz/tq")))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let err = source(&server)
        .fetch_csv()
        .await
        .expect_err("a 500 is unreachable, not a parse failure");
    assert_eq!(err.slug(), "sheet-unreachable");
}

#[tokio::test]
async fn a_timeout_is_unreachable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/spreadsheets/d/{ID}/gviz/tq")))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(400)))
        .mount(&server)
        .await;

    let short_timeout = GoogleSheetSource::new(&sheet_ref(), Duration::from_millis(50))
        .unwrap()
        .with_base_url(server.uri());

    let err = short_timeout
        .fetch_csv()
        .await
        .expect_err("a client timeout is unreachable");
    assert_eq!(err.slug(), "sheet-unreachable");
}

#[tokio::test]
async fn an_oversize_body_is_unreachable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/spreadsheets/d/{ID}/gviz/tq")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/csv")
                .set_body_string(sample_csv()),
        )
        .mount(&server)
        .await;

    let tiny_limit = source(&server).with_max_bytes(64);
    let err = tiny_limit
        .fetch_csv()
        .await
        .expect_err("a body past the ceiling is unreachable, not parsed");
    assert_eq!(err.slug(), "sheet-unreachable");
}

#[tokio::test]
async fn a_csv_missing_the_trade_column_is_refused() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/spreadsheets/d/{ID}/gviz/tq")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/csv")
                .set_body_string("Side,Qty,OCC Symbol\nSELL,1,SPX260918C8200\n"),
        )
        .mount(&server)
        .await;

    let err = source(&server)
        .load(&options())
        .await
        .expect_err("a Legs table with no Trade column cannot be imported");
    assert_eq!(err.slug(), "invalid-trade-source");
    assert!(err.detail().contains("Trade"), "{err:?}");
}

/// A `MakeWriter` that keeps every byte written to it, so a test can inspect
/// exactly what `tracing` emitted rather than trusting that nothing slipped
/// through by construction.
#[derive(Clone)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Sink {
    type Writer = Sink;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn nothing_about_the_id_leaks_into_labels_debug_or_logs() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/spreadsheets/d/{ID}/gviz/tq")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/csv")
                .set_body_string(sample_csv()),
        )
        .mount(&server)
        .await;

    let sink = Sink(Arc::new(Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(sink.clone())
        .with_max_level(tracing::Level::TRACE)
        .finish();

    let (src, label, debug) = {
        let _guard = tracing::subscriber::set_default(subscriber);
        let src = source(&server);
        let label = src.label();
        let debug = format!("{src:?}");
        src.load(&options())
            .await
            .expect("the happy path still succeeds under the tracing guard");
        (src, label, debug)
    };
    assert_eq!(src.kind(), SourceKind::Sheet);

    let logs = String::from_utf8(sink.0.lock().unwrap().clone()).expect("tracing output is UTF-8");

    for haystack in [&label, &debug, &logs] {
        assert!(
            !haystack.contains(ID),
            "the full spreadsheet id leaked: {haystack:?}"
        );
    }
    // Masking, not omission: the id's fingerprint is still visible.
    assert!(
        label.contains("1AbCdE\u{2026}abcd"),
        "label should carry the masked id: {label:?}"
    );
}
