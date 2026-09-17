//! Turning whatever a trader pastes into the Trade source dialog — a full
//! share link, an `/edit`, a `/pub`, a `gviz/tq` or `/export` URL, or a bare
//! spreadsheet id — into the one thing Wingspan actually needs: the
//! spreadsheet id, whether it is a *published* document (a different fetch
//! shape, no tab-by-name selection), and the `gid` of the tab the link
//! happened to be pointing at.
//!
//! Pure and synchronous: no I/O, no async, nothing here ever makes a
//! request. [`crate::source::sheet::GoogleSheetSource`] is what fetches;
//! this module only parses the input and builds the URLs it fetches from,
//! and masks the id everywhere the id must not appear in full — the share
//! link is bearer-equivalent, exactly like `[schwab] token_path`.
//!
//! We lean on [`reqwest::Url`] (a re-export of the `url` crate) for both
//! parsing the pasted text and building query strings, rather than hand-
//! rolling percent-encoding — a tab named `My Legs` has to survive the round
//! trip, and getting that right by hand is exactly the kind of thing a
//! maintained URL crate already does correctly.

use reqwest::Url;

/// The only host Wingspan will ever fetch a sheet from. Checked on every
/// pasted link — a URL naming any other host is refused before Rust builds a
/// single request from it, which is also what keeps a look-alike phishing
/// link from ever being fetched.
///
/// Checking the *pasted* link would not be enough on its own: a redirect can
/// move a request to another host after it is built, and the spreadsheet id
/// travels in the request path rather than in a header, so `reqwest`'s own
/// cross-host header stripping would not protect it. See
/// [`crate::source::sheet::redirect_policy`], which follows a redirect only
/// back to the host the request started on, or to [`SIGNIN_HOST`].
pub const HOST: &str = "docs.google.com";

/// Where Google sends an unauthenticated request for a sheet that is not
/// shared. Reaching it is the signal [`crate::source::sheet::classify`] reads
/// as `sheet-not-shared`, so it is the one host other than the request's own
/// that a redirect may lead to.
pub const SIGNIN_HOST: &str = "accounts.google.com";

/// The real Google Sheets host, used to build every fetch and browser URL
/// unless a caller overrides it (tests point `fetch_url`/`fetch_url_by_gid`
/// at a local mock server; `browser_url` never takes the override — a link
/// handed to the user's actual browser must always be the real host).
pub const DEFAULT_BASE: &str = "https://docs.google.com";

/// Google Drive ids are 40+ chars; 20 is the floor Wingspan accepts, and it
/// is deliberately the same 20 that `scripts/check_redaction.py`'s
/// `spreadsheets/d/<20+ chars>` guard scans for, so no id can be short
/// enough to be legal here and invisible to the guard.
pub const MIN_ID_LEN: usize = 20;

/// A spreadsheet link boiled down to what Wingspan needs to fetch it.
///
/// `Debug` is hand-written for the same reason [`crate::config::TradesConfig`]'s
/// is: `id` is bearer-equivalent, and a derived `Debug` would put it whole
/// into the first `tracing::debug!` written against this type.
#[derive(Clone, PartialEq, Eq)]
pub struct ParsedSheet {
    /// The document id from `/d/<id>/`, or the publish id from
    /// `/d/e/<pubid>/pub`. Never logged, never sent to the SPA — see
    /// [`mask_id`].
    pub id: String,
    /// `true` for a `File > Share > Publish to web` link (`/d/e/<id>/pub`):
    /// fetched differently, and a published link cannot select a tab by
    /// name (there is no header row of tab names to ask gviz for).
    pub published: bool,
    /// The `gid` the pasted link carried, when it carried one. `None` is
    /// not an error — most links to the first tab of a spreadsheet have no
    /// `gid` at all.
    pub gid: Option<u64>,
}

impl std::fmt::Debug for ParsedSheet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParsedSheet")
            .field("id", &mask_id(&self.id))
            .field("published", &self.published)
            .field("gid", &self.gid)
            .finish()
    }
}

/// Why a pasted string could not become a [`ParsedSheet`]. Every variant's
/// `Display` is shown to the trader verbatim in the Trade source dialog, so
/// each one names the specific thing that was wrong rather than saying
/// "invalid URL".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UrlError {
    #[error("paste the sheet's share link, or its id")]
    Empty,
    #[error("Wingspan only reads Google Sheets — {0} is not docs.google.com")]
    WrongHost(String),
    #[error("that link has no spreadsheet id in it")]
    NoId,
    #[error("that does not look like a spreadsheet id")]
    BadId,
}

/// `true` when every character is one a Google Drive id can legally contain
/// (`[A-Za-z0-9_-]`) and there are at least [`MIN_ID_LEN`] of them.
fn looks_like_id(candidate: &str) -> bool {
    candidate.len() >= MIN_ID_LEN
        && candidate
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Pull the `gid=<digits>` pair out of a fragment (`gid=123&range=A1:B2`),
/// tolerating a fragment that carries other keys or none at all. A `gid`
/// present but not entirely digits is treated as absent rather than as a
/// parse error — a malformed fragment on an otherwise-good link should not
/// block the import over a tab id nobody typed by hand.
fn gid_from_fragment(fragment: &str) -> Option<u64> {
    fragment.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == "gid").then(|| value.parse().ok()).flatten()
    })
}

/// Extract a spreadsheet id (and, for a `/d/e/<id>/pub` link, the published
/// flag) from a URL's path segments. `None` means the path had no `d`
/// segment, or nothing followed it — [`parse`] turns that into
/// [`UrlError::NoId`].
fn id_from_path(url: &Url) -> Option<(String, bool)> {
    let segments: Vec<&str> = url.path_segments()?.collect();
    let d_at = segments.iter().position(|s| *s == "d")?;
    let after_d = segments.get(d_at + 1)?;
    if *after_d == "e" {
        let pub_id = segments.get(d_at + 2)?;
        Some(((*pub_id).to_string(), true))
    } else {
        Some(((*after_d).to_string(), false))
    }
}

/// Parse a pasted share link or bare id into a [`ParsedSheet`].
///
/// Accepts, from this one code path, every shape `docs/DATA-FORMATS.md`
/// documents: an `/edit?gid=` or `/edit#gid=` link, a
/// `/gviz/tq?tqx=out:csv&sheet=Legs` URL, an `/export?format=csv&gid=` URL,
/// a published `/d/e/<pubid>/pub?output=csv&gid=` link, and a bare
/// spreadsheet id with no URL around it at all.
pub fn parse(input: &str) -> Result<ParsedSheet, UrlError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(UrlError::Empty);
    }

    // A bare id: nothing that looks like a URL around it at all.
    if !trimmed.contains('/') && !trimmed.contains("://") {
        return if looks_like_id(trimmed) {
            Ok(ParsedSheet {
                id: trimmed.to_string(),
                published: false,
                gid: None,
            })
        } else {
            Err(UrlError::BadId)
        };
    }

    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let url = Url::parse(&with_scheme).map_err(|_| UrlError::NoId)?;

    let host = url.host_str().unwrap_or("that address");
    if host != HOST {
        return Err(UrlError::WrongHost(host.to_string()));
    }

    let (id, published) = id_from_path(&url).ok_or(UrlError::NoId)?;
    if !looks_like_id(&id) {
        return Err(UrlError::BadId);
    }

    let gid = url
        .query_pairs()
        .find(|(k, _)| k == "gid")
        .and_then(|(_, v)| v.parse().ok())
        .or_else(|| url.fragment().and_then(gid_from_fragment));

    Ok(ParsedSheet { id, published, gid })
}

/// Build the CSV-fetch URL for `tab` by name. `base` is [`DEFAULT_BASE`] in
/// production and a mock server's URL in tests
/// ([`crate::source::sheet::GoogleSheetSource::with_base_url`]).
///
/// A published sheet has no header row of tab names to ask gviz for, so it
/// is fetched by `output=csv` instead (plus the `gid` when the link carried
/// one — a published spreadsheet with more than one tab still needs it to
/// pick anything but the first).
#[must_use]
pub fn fetch_url(base: &str, sheet: &ParsedSheet, tab: &str) -> String {
    if sheet.published {
        return published_url(base, &sheet.id, sheet.gid);
    }
    gviz_url(base, &sheet.id, "sheet", tab)
}

/// Build the CSV-fetch URL by `gid` instead of by tab name — used on a
/// reload, once the tab a source was chosen against is already known to
/// exist, so a tab renamed after import still resolves to the same sheet.
#[must_use]
pub fn fetch_url_by_gid(base: &str, sheet: &ParsedSheet, gid: u64) -> String {
    if sheet.published {
        return published_url(base, &sheet.id, Some(gid));
    }
    gviz_url(base, &sheet.id, "gid", &gid.to_string())
}

/// `{base}/spreadsheets/d/e/{id}/pub?output=csv[&gid={gid}]`.
fn published_url(base: &str, id: &str, gid: Option<u64>) -> String {
    let path = format!("{base}/spreadsheets/d/e/{id}/pub");
    match Url::parse(&path) {
        Ok(mut url) => {
            url.query_pairs_mut().append_pair("output", "csv");
            if let Some(gid) = gid {
                url.query_pairs_mut().append_pair("gid", &gid.to_string());
            }
            url.into()
        }
        // `base` is always either `DEFAULT_BASE` or a URL a caller already
        // validated (`with_base_url`); this is an unreachable-in-practice
        // fallback that still never panics.
        Err(_) => match gid {
            Some(gid) => format!("{path}?output=csv&gid={gid}"),
            None => format!("{path}?output=csv"),
        },
    }
}

/// `{base}/spreadsheets/d/{id}/gviz/tq?tqx=out:csv&{key}={value}`, with
/// `value` percent-encoded by [`Url::query_pairs_mut`] so a tab named
/// `My Legs` survives the round trip.
fn gviz_url(base: &str, id: &str, key: &str, value: &str) -> String {
    let path = format!("{base}/spreadsheets/d/{id}/gviz/tq");
    match Url::parse(&path) {
        Ok(mut url) => {
            url.query_pairs_mut()
                .append_pair("tqx", "out:csv")
                .append_pair(key, value);
            url.into()
        }
        Err(_) => format!("{path}?tqx=out:csv&{key}={value}"),
    }
}

/// The link to hand a "open in your browser" affordance — always the real
/// `docs.google.com`, never `base` (a fetch may be aimed at a mock server in
/// tests; a link the user is meant to click never is).
#[must_use]
pub fn browser_url(sheet: &ParsedSheet) -> String {
    if sheet.published {
        format!("{DEFAULT_BASE}/spreadsheets/d/e/{}/pub", sheet.id)
    } else {
        match sheet.gid {
            Some(gid) => format!("{DEFAULT_BASE}/spreadsheets/d/{}/edit#gid={gid}", sheet.id),
            None => format!("{DEFAULT_BASE}/spreadsheets/d/{}/edit", sheet.id),
        }
    }
}

/// Mask a spreadsheet id for display: first 6 chars, an ellipsis, last 4.
/// An id of 10 characters or fewer is masked down to just the ellipsis —
/// showing 6+4 of a 10-char id would reveal most of it, and a short id is
/// exactly the case where every character matters.
#[must_use]
pub fn mask_id(id: &str) -> String {
    let chars: Vec<char> = id.chars().collect();
    if chars.len() <= 10 {
        return "\u{2026}".to_string();
    }
    let head: String = chars[..6].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}\u{2026}{tail}")
}

/// The label shown everywhere a chosen sheet is named: `trade_source`,
/// `app_config`, the recent-sources list, and every log line that would
/// otherwise be tempted to print the id. `tab` is the tab name when one
/// applies (never for a CSV or folder source, which don't call this at
/// all).
#[must_use]
pub fn mask(id: &str, tab: Option<&str>) -> String {
    let masked = mask_id(id);
    match tab {
        Some(tab) => format!("{HOST}/\u{2026}/{masked} \u{b7} {tab}"),
        None => format!("{HOST}/\u{2026}/{masked}"),
    }
}

/// Mask whatever a trader pasted, without a tab name — used where only the
/// raw input is in hand (a config-file `sheet_url`, before it is even known
/// to parse). A string that fails to parse is still potentially a
/// credential — a share link with a typo is a share link — so it is never
/// echoed back in any form; `"(set)"` says only that *something* is there.
#[must_use]
pub fn mask_input(input: &str) -> String {
    match parse(input) {
        Ok(sheet) => mask(&sheet.id, None),
        Err(_) => "(set)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one id used in every test and every fixture in this wave —
    /// never the user's real spreadsheet id.
    const ID: &str = "1AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcd";
    const GID: u64 = 123_456_789;

    #[test]
    fn debug_of_a_parsed_sheet_masks_the_id() {
        let printed = format!("{:?}", parse(ID).unwrap());
        assert!(!printed.contains(ID), "{printed}");
        assert!(printed.contains("1AbCdE\u{2026}abcd"), "{printed}");
    }

    #[test]
    fn a_bare_id_parses_with_no_gid_and_not_published() {
        let sheet = parse(ID).unwrap();
        assert_eq!(sheet.id, ID);
        assert!(!sheet.published);
        assert_eq!(sheet.gid, None);
    }

    #[test]
    fn an_edit_link_with_a_query_gid_parses() {
        let sheet = parse(&format!(
            "https://docs.google.com/spreadsheets/d/{ID}/edit?gid={GID}"
        ))
        .unwrap();
        assert_eq!(sheet.id, ID);
        assert!(!sheet.published);
        assert_eq!(sheet.gid, Some(GID));
    }

    #[test]
    fn an_edit_link_with_a_fragment_gid_parses() {
        let sheet = parse(&format!(
            "https://docs.google.com/spreadsheets/d/{ID}/edit#gid={GID}"
        ))
        .unwrap();
        assert_eq!(sheet.id, ID);
        assert!(!sheet.published);
        assert_eq!(sheet.gid, Some(GID));
    }

    #[test]
    fn a_gviz_csv_link_parses() {
        let sheet = parse(&format!(
            "https://docs.google.com/spreadsheets/d/{ID}/gviz/tq?tqx=out:csv&sheet=Legs"
        ))
        .unwrap();
        assert_eq!(sheet.id, ID);
        assert!(!sheet.published);
        assert_eq!(sheet.gid, None);
    }

    #[test]
    fn an_export_link_with_a_gid_parses() {
        let sheet = parse(&format!(
            "https://docs.google.com/spreadsheets/d/{ID}/export?format=csv&gid={GID}"
        ))
        .unwrap();
        assert_eq!(sheet.id, ID);
        assert!(!sheet.published);
        assert_eq!(sheet.gid, Some(GID));
    }

    #[test]
    fn a_published_link_parses_as_published_with_its_gid() {
        let sheet = parse(&format!(
            "https://docs.google.com/spreadsheets/d/e/{ID}/pub?output=csv&gid={GID}"
        ))
        .unwrap();
        assert_eq!(sheet.id, ID);
        assert!(sheet.published);
        assert_eq!(sheet.gid, Some(GID));
    }

    #[test]
    fn a_scheme_less_link_is_accepted_the_same_as_an_https_one() {
        let sheet = parse(&format!("docs.google.com/spreadsheets/d/{ID}/edit")).unwrap();
        assert_eq!(sheet.id, ID);
    }

    #[test]
    fn a_non_google_host_is_wrong_host() {
        let err = parse(&format!("http://evil.example/spreadsheets/d/{ID}/edit")).unwrap_err();
        assert_eq!(err, UrlError::WrongHost("evil.example".to_string()));
    }

    #[test]
    fn empty_and_blank_input_is_empty() {
        assert_eq!(parse(""), Err(UrlError::Empty));
        assert_eq!(parse("   "), Err(UrlError::Empty));
    }

    #[test]
    fn nonsense_and_a_too_short_bare_id_are_bad_id() {
        assert_eq!(parse("not a link"), Err(UrlError::BadId));
        assert_eq!(parse("tooshort123"), Err(UrlError::BadId));
    }

    #[test]
    fn a_google_doc_link_is_not_a_spreadsheet_link() {
        // Pinned to whatever this code actually gives: `document` is not
        // `d`, so path-segment matching still finds the `d` at index 2 and
        // takes the id after it. Either `BadId` or `NoId` is an acceptable
        // shape for this input; this locks in the one this code produces.
        let err = parse("https://docs.google.com/document/d/x/edit").unwrap_err();
        assert_eq!(err, UrlError::BadId);
    }

    #[test]
    fn fetch_url_for_a_plain_sheet_uses_gviz_with_the_tab_name() {
        let sheet = parse(ID).unwrap();
        let url = fetch_url(DEFAULT_BASE, &sheet, "Legs");
        assert_eq!(
            url,
            format!("https://docs.google.com/spreadsheets/d/{ID}/gviz/tq?tqx=out%3Acsv&sheet=Legs")
        );
    }

    #[test]
    fn fetch_url_percent_encodes_a_tab_name_with_a_space() {
        let sheet = parse(ID).unwrap();
        let url = fetch_url(DEFAULT_BASE, &sheet, "My Legs");
        assert!(
            url.contains("sheet=My+Legs") || url.contains("sheet=My%20Legs"),
            "{url}"
        );
    }

    #[test]
    fn fetch_url_for_a_published_sheet_uses_the_pub_shape() {
        let mut sheet = parse(ID).unwrap();
        sheet.published = true;
        sheet.gid = Some(GID);
        let url = fetch_url(DEFAULT_BASE, &sheet, "Legs");
        assert_eq!(
            url,
            format!("https://docs.google.com/spreadsheets/d/e/{ID}/pub?output=csv&gid={GID}")
        );
    }

    #[test]
    fn fetch_url_by_gid_uses_gid_instead_of_sheet_name() {
        let sheet = parse(ID).unwrap();
        let url = fetch_url_by_gid(DEFAULT_BASE, &sheet, GID);
        assert_eq!(
            url,
            format!("https://docs.google.com/spreadsheets/d/{ID}/gviz/tq?tqx=out%3Acsv&gid={GID}")
        );
    }

    #[test]
    fn browser_url_always_names_the_real_host_even_with_a_local_base() {
        let sheet = parse(ID).unwrap();
        // `fetch_url` would honour a mock base; `browser_url` never does.
        let fetched = fetch_url("http://127.0.0.1:9", &sheet, "Legs");
        assert!(fetched.starts_with("http://127.0.0.1:9/"), "{fetched}");
        assert_eq!(
            browser_url(&sheet),
            format!("https://docs.google.com/spreadsheets/d/{ID}/edit")
        );
    }

    #[test]
    fn browser_url_carries_the_gid_and_the_published_shape() {
        let mut sheet = parse(ID).unwrap();
        sheet.gid = Some(GID);
        assert_eq!(
            browser_url(&sheet),
            format!("https://docs.google.com/spreadsheets/d/{ID}/edit#gid={GID}")
        );
        sheet.published = true;
        assert_eq!(
            browser_url(&sheet),
            format!("https://docs.google.com/spreadsheets/d/e/{ID}/pub")
        );
    }

    #[test]
    fn mask_id_on_a_short_id_shows_only_the_ellipsis() {
        assert_eq!(mask_id(""), "\u{2026}");
        assert_eq!(mask_id("short"), "\u{2026}");
        assert_eq!(mask_id("exactlyten"), "\u{2026}");
    }

    #[test]
    fn mask_id_on_the_placeholder_shows_six_and_four() {
        assert_eq!(mask_id(ID), "1AbCdE\u{2026}abcd");
    }

    #[test]
    fn mask_without_a_tab_has_no_middle_dot() {
        assert_eq!(
            mask(ID, None),
            "docs.google.com/\u{2026}/1AbCdE\u{2026}abcd"
        );
    }

    #[test]
    fn mask_with_a_tab_matches_the_ipc_doc_exactly() {
        assert_eq!(
            mask(ID, Some("Legs")),
            "docs.google.com/\u{2026}/1AbCdE\u{2026}abcd \u{b7} Legs"
        );
    }

    #[test]
    fn mask_input_masks_a_parseable_link_and_hides_garbage_entirely() {
        assert_eq!(
            mask_input(&format!("https://docs.google.com/spreadsheets/d/{ID}/edit")),
            "docs.google.com/\u{2026}/1AbCdE\u{2026}abcd"
        );
        assert_eq!(mask_input("not a link"), "(set)");
        assert_eq!(mask_input(""), "(set)");
    }
}
