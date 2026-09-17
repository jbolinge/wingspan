//! The Google Sheet trade source: an unauthenticated `gviz` fetch of the
//! Legs tab, classified into a book or a slug-shaped [`SourceError`].
//!
//! The sheet is shared *Anyone with the link → Viewer*, so there is no OAuth
//! flow here — just a `GET` and a careful reading of what came back. Google
//! answers an unshared or renamed sheet with ordinary-looking HTTP (a
//! redirect to a sign-in page, or a 200 carrying `gviz`'s own error payload),
//! so [`classify`] is the part worth testing on its own: it is a pure
//! function from "what the wire said" to [`Verdict`], with no network, no
//! clock and no id in sight.
//!
//! The spreadsheet id is bearer-equivalent — anyone who has it can read the
//! sheet — so it never appears in a log line, an error sentence, or this
//! type's [`std::fmt::Debug`]. Every place that would otherwise want the raw
//! id uses [`crate::sheet_url::mask`] instead.

use std::time::Duration;

use chrono::Utc;
use wingspan_core::book::LoadOptions;
use wingspan_core::legs::LegSourceInfo;
use wingspan_core::sheet::RowOrigin;

use crate::sheet_url::{self, ParsedSheet};
use crate::source::{self, SourceError, SourceKind, SourceLoad, SourceRef, TradeSource};

/// The sentence shown when the sheet is not readable by an anonymous viewer —
/// either because it was never shared that way, or because sharing was
/// revoked. Frozen: it reaches the dialog verbatim as `detail`.
const NOT_SHARED_DETAIL: &str =
    "that sheet is not shared — set sharing to Anyone with the link → Viewer, then try again";

/// The biggest chunk of a response body [`classify`] needs to see. Reading
/// more than this to decide "is this HTML, CSV, or gviz's error payload"
/// would be wasted work — and it lets [`GoogleSheetSource::fetch_csv`] make
/// its call before the whole (potentially oversize) body is even collected.
const CLASSIFY_SNIFF_BYTES: usize = 4096;

/// One Google Sheet, resolved to a fetchable URL.
///
/// Built from a [`SourceRef::Sheet`]; every other variant is refused by
/// [`GoogleSheetSource::new`]. The HTTP client is built once, here, and kept
/// — see `crates/wingspan-marketdata/src/schwab/mod.rs`'s `Schwab::new` for
/// the same pattern applied to a different provider.
pub struct GoogleSheetSource {
    id: String,
    tab: String,
    gid: Option<u64>,
    published: bool,
    /// Overridable so a test can point the fetch at a `wiremock` server
    /// instead of the real `docs.google.com`. `browser_url` ignores this —
    /// the link handed to a real browser is always the genuine one.
    base: String,
    timeout: Duration,
    max_bytes: usize,
    http: reqwest::Client,
}

impl GoogleSheetSource {
    /// Fails with [`SourceError::Invalid`] when `reference` is not a sheet —
    /// callers are expected to have already matched on [`SourceRef`], so this
    /// is a defensive rather than an expected path.
    pub fn new(reference: &SourceRef, timeout: Duration) -> Result<Self, SourceError> {
        let SourceRef::Sheet {
            id,
            tab,
            gid,
            published,
        } = reference
        else {
            return Err(SourceError::Invalid(
                "not a Google Sheet source".to_string(),
            ));
        };
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .user_agent(concat!("wingspan/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        Ok(Self {
            id: id.clone(),
            tab: tab.clone(),
            gid: *gid,
            published: *published,
            base: sheet_url::DEFAULT_BASE.to_string(),
            timeout,
            max_bytes: source::MAX_CSV_BYTES,
            http,
        })
    }

    /// Point the fetch at another origin — the localhost server the tests
    /// run. Never used outside a test: production always fetches the real
    /// `docs.google.com`.
    #[doc(hidden)]
    #[must_use]
    pub fn with_base_url(mut self, base: impl Into<String>) -> Self {
        self.base = base.into();
        self
    }

    /// Lower the size ceiling so a test does not have to serve 8 MB to prove
    /// the oversize path works.
    #[doc(hidden)]
    #[must_use]
    pub fn with_max_bytes(mut self, max: usize) -> Self {
        self.max_bytes = max;
        self
    }

    fn parsed(&self) -> ParsedSheet {
        ParsedSheet {
            id: self.id.clone(),
            published: self.published,
            gid: self.gid,
        }
    }

    /// The masked, human-readable label — safe for a DTO, a log line, or
    /// this type's [`std::fmt::Debug`]. NEVER the full spreadsheet id.
    fn masked_label(&self) -> String {
        sheet_url::mask(&self.id, Some(&self.tab))
    }

    /// The link `open_sheet` hands the browser — always the genuine
    /// `docs.google.com`, regardless of [`Self::with_base_url`].
    #[must_use]
    pub fn browser_url(&self) -> String {
        sheet_url::browser_url(&self.parsed())
    }

    /// One request, fully read and classified. Never retries — the one
    /// retry this source ever does (a `gid=`-addressed request after a
    /// `sheet=`-addressed one comes back tab-not-found) is orchestrated by
    /// [`Self::fetch_csv`], which calls this twice when it needs to.
    async fn attempt(&self, url: &str) -> Result<Attempt, SourceError> {
        let response = self.http.get(url).send().await.map_err(|err| {
            SourceError::Unreachable(format!(
                "could not reach docs.google.com ({})",
                err.without_url()
            ))
        })?;

        let final_url = response.url().as_str().to_string();
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();

        // A declared size past the ceiling fails before a single body byte
        // is read — no reason to pull down megabytes we are going to refuse.
        if response
            .content_length()
            .is_some_and(|len| len as usize > self.max_bytes)
        {
            return Err(oversize_error());
        }

        let bytes = response.bytes().await.map_err(|err| {
            SourceError::Unreachable(format!(
                "could not reach docs.google.com ({})",
                err.without_url()
            ))
        })?;
        if bytes.len() > self.max_bytes {
            return Err(oversize_error());
        }

        let sniff_len = bytes.len().min(CLASSIFY_SNIFF_BYTES);
        let sniff = String::from_utf8_lossy(&bytes[..sniff_len]);
        match classify(status, &final_url, &content_type, &sniff) {
            Verdict::Csv => {
                let text = String::from_utf8(bytes.to_vec()).map_err(|_| {
                    SourceError::Invalid("that sheet did not answer UTF-8 text".to_string())
                })?;
                Ok(Attempt::Csv(text))
            }
            Verdict::NotShared => Ok(Attempt::NotShared),
            Verdict::TabNotFound => Ok(Attempt::TabNotFound),
            Verdict::Unreachable => Err(SourceError::Unreachable(format!(
                "could not reach docs.google.com (HTTP {status})"
            ))),
        }
    }

    /// The bytes of the Legs tab, or the slug-shaped error that says why
    /// not. On a tab-not-found result, and only when the reference carries a
    /// `gid` (meaning a fetch has already succeeded here once before), retry
    /// once against the `gid=`-addressed URL — a sheet tab can be renamed
    /// without changing its `gid`, so the retry recovers from that without
    /// ever bothering the user.
    pub async fn fetch_csv(&self) -> Result<String, SourceError> {
        tracing::info!(source = %self.masked_label(), "fetching the Legs tab");

        let url = sheet_url::fetch_url(&self.base, &self.parsed(), &self.tab);
        let result = match self.attempt(&url).await {
            Ok(Attempt::Csv(text)) => Ok(text),
            Ok(Attempt::NotShared) => Err(SourceError::NotShared(NOT_SHARED_DETAIL.to_string())),
            Ok(Attempt::TabNotFound) => match self.gid {
                Some(gid) => {
                    let retry_url = sheet_url::fetch_url_by_gid(&self.base, &self.parsed(), gid);
                    match self.attempt(&retry_url).await {
                        Ok(Attempt::Csv(text)) => Ok(text),
                        Ok(Attempt::NotShared) => {
                            Err(SourceError::NotShared(NOT_SHARED_DETAIL.to_string()))
                        }
                        Ok(Attempt::TabNotFound) => Err(tab_not_found_error(&self.tab)),
                        Err(err) => Err(err),
                    }
                }
                None => Err(tab_not_found_error(&self.tab)),
            },
            Err(err) => Err(err),
        };

        if let Err(ref err) = result {
            tracing::warn!(
                err = err.slug(),
                source = %self.masked_label(),
                "fetching the Legs tab failed"
            );
        }
        result
    }
}

impl std::fmt::Debug for GoogleSheetSource {
    /// Only the masked label, the tab and the timeout — never the id.
    /// `TradeSource` requires `Debug` as a supertrait precisely so a derive
    /// elsewhere can never put the id into a `tracing::debug!` by accident.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleSheetSource")
            .field("label", &self.masked_label())
            .field("tab", &self.tab)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl TradeSource for GoogleSheetSource {
    fn kind(&self) -> SourceKind {
        SourceKind::Sheet
    }

    fn label(&self) -> String {
        self.masked_label()
    }

    fn reference(&self) -> Option<SourceRef> {
        Some(SourceRef::Sheet {
            id: self.id.clone(),
            tab: self.tab.clone(),
            gid: self.gid,
            published: self.published,
        })
    }

    async fn load(&self, options: &LoadOptions) -> Result<SourceLoad, SourceError> {
        let load = self.load_for_probe(options).await?;
        if let Some(summary) = load.summary.as_ref() {
            source::refuse_if_columns_missing(summary)?;
        }
        Ok(load)
    }

    /// The same fetch and parse, without the refusal — see
    /// [`TradeSource::load_for_probe`]: a probe reports a missing required
    /// column in `columns_missing` rather than declining to answer.
    async fn load_for_probe(&self, options: &LoadOptions) -> Result<SourceLoad, SourceError> {
        let text = self.fetch_csv().await?;
        let info = LegSourceInfo {
            label: self.masked_label(),
            as_of: options
                .valuation_date
                .unwrap_or_else(|| Utc::now().date_naive()),
            timezone: options.timezone,
        };
        let (book, summary, warnings) =
            source::book_from_csv(&text, RowOrigin::Sheet, info, options);
        Ok(SourceLoad {
            book,
            summary: Some(summary),
            warnings,
            fetched_at: Utc::now(),
            from_cache: false,
            note: None,
        })
    }
}

/// The outcome of one HTTP attempt, once classified — everything an attempt
/// needs to report other than an outright [`SourceError`] (a transport
/// failure, an oversize body, or an unrecognisable status all become one of
/// those directly, since there is nothing left to retry for them).
enum Attempt {
    Csv(String),
    NotShared,
    TabNotFound,
}

fn oversize_error() -> SourceError {
    SourceError::Unreachable("could not reach docs.google.com (response too large)".to_string())
}

fn tab_not_found_error(tab: &str) -> SourceError {
    SourceError::TabNotFound(format!("no tab named {tab:?} in that spreadsheet"))
}

/// What a finished response means, judged only by what is visible on the
/// wire. Pure and total, so the whole table is a unit test — the network
/// path in [`GoogleSheetSource::fetch_csv`] exists only to get these four
/// values in front of it.
///
/// The rules are applied in order; the first one that matches wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// A Legs CSV, ready to parse.
    Csv,
    /// Google would rather show the requester a sign-in page than the sheet.
    NotShared,
    /// The sheet answered, but not with the tab that was asked for.
    TabNotFound,
    /// Nothing above matches; the sheet or the network is simply unwell.
    Unreachable,
}

/// Classify one finished HTTP response.
///
/// 1. A redirect to Google's sign-in flow — `final_url` is where `reqwest`
///    ended up after following every redirect, and Google 302s an
///    unauthenticated request there rather than answering 403 (observed live
///    against the real sheet).
/// 2. `401`/`403` — the more direct way of saying the same thing.
/// 3. `5xx` — the sheet or Google itself is unwell.
/// 4. Any other non-`2xx` — unreachable too; the caller puts the status
///    in the sentence.
/// 5. A `2xx` whose body is `gviz`'s own error payload — served with status
///    `200`, so status alone cannot tell this apart from a real CSV.
/// 6. A `2xx` `text/html` body — an interstitial or consent page, not data.
/// 7. Otherwise: a CSV.
#[must_use]
pub fn classify(status: u16, final_url: &str, content_type: &str, body: &str) -> Verdict {
    if final_url.contains("accounts.google.com")
        || final_url.contains("/ServiceLogin")
        || final_url.contains("/signin")
    {
        return Verdict::NotShared;
    }
    if status == 401 || status == 403 {
        return Verdict::NotShared;
    }
    if status >= 500 {
        return Verdict::Unreachable;
    }
    if !(200..300).contains(&status) {
        return Verdict::Unreachable;
    }

    let trimmed = body.trim();
    if trimmed.starts_with("/*O_o*/")
        || body.contains("\"status\":\"error\"")
        || trimmed.to_lowercase().contains("invalid sheet name")
    {
        return Verdict::TabNotFound;
    }
    if content_type.to_lowercase().contains("text/html") {
        return Verdict::NotShared;
    }
    Verdict::Csv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_login_redirect_is_not_shared() {
        assert_eq!(
            classify(200, "https://accounts.google.com/signin", "text/html", ""),
            Verdict::NotShared
        );
        assert_eq!(
            classify(
                200,
                "https://docs.google.com/ServiceLogin?x=1",
                "text/html",
                ""
            ),
            Verdict::NotShared
        );
        assert_eq!(
            classify(200, "https://docs.google.com/signin/v2", "text/html", ""),
            Verdict::NotShared
        );
    }

    #[test]
    fn a_401_or_403_is_not_shared() {
        assert_eq!(
            classify(401, "https://docs.google.com/x", "text/plain", ""),
            Verdict::NotShared
        );
        assert_eq!(
            classify(403, "https://docs.google.com/x", "text/plain", ""),
            Verdict::NotShared
        );
    }

    #[test]
    fn server_errors_are_unreachable() {
        assert_eq!(
            classify(500, "https://docs.google.com/x", "text/plain", ""),
            Verdict::Unreachable
        );
        assert_eq!(
            classify(503, "https://docs.google.com/x", "text/plain", ""),
            Verdict::Unreachable
        );
    }

    #[test]
    fn any_other_non_2xx_is_unreachable() {
        assert_eq!(
            classify(404, "https://docs.google.com/x", "text/plain", ""),
            Verdict::Unreachable
        );
        assert_eq!(
            classify(301, "https://docs.google.com/x", "text/plain", ""),
            Verdict::Unreachable
        );
    }

    #[test]
    fn gvizs_own_error_payload_is_tab_not_found_even_at_200() {
        let body = "/*O_o*/\ngoogle.visualization.Query.setResponse({\"version\":\"0.6\",\"status\":\"error\",\"errors\":[{\"reason\":\"invalid_query\",\"message\":\"INVALID_QUERY\",\"detailed_message\":\"Invalid sheet name\"}]});";
        assert_eq!(
            classify(200, "https://docs.google.com/x", "text/javascript", body),
            Verdict::TabNotFound
        );
        // Any of the three signals is sufficient on its own.
        assert_eq!(
            classify(
                200,
                "https://docs.google.com/x",
                "text/plain",
                "  invalid sheet NAME here"
            ),
            Verdict::TabNotFound
        );
        assert_eq!(
            classify(
                200,
                "https://docs.google.com/x",
                "text/plain",
                "{\"status\":\"error\"}"
            ),
            Verdict::TabNotFound
        );
    }

    #[test]
    fn a_2xx_html_body_is_not_shared() {
        assert_eq!(
            classify(
                200,
                "https://docs.google.com/x",
                "text/html; charset=utf-8",
                "<html>consent</html>"
            ),
            Verdict::NotShared
        );
    }

    #[test]
    fn a_plain_2xx_body_is_csv() {
        assert_eq!(
            classify(
                200,
                "https://docs.google.com/x",
                "text/csv",
                "Trade,Side,Qty,OCC Symbol\n"
            ),
            Verdict::Csv
        );
    }
}
