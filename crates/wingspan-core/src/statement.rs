//! thinkorswim account-statement CSVs.
//!
//! These are the only source of per-leg fill prices, execution times,
//! commissions and end-of-day marks, and they are a minefield:
//!
//! * UTF-8 BOM, CRLF, and sections identified by their **header names** —
//!   *not* by blank lines: there is a line containing a single space and a
//!   stray `"Total Cash …"` row that moves between sections from file to file.
//! * Excel refs (`="9000000000001"`), quoted thousands, parenthesised
//!   negatives, `$` prefixes, leading-dot decimals (`.725`), `--`/`N/A`/`~`
//!   sentinels.
//! * Multi-row spreads whose continuation rows have an empty `Exec Time`, and
//!   an `RE #…` replace-order reference that lands in the `Spread` column of
//!   an *Order History* continuation row.
//! * `CANCELED` orders whose legs are identical to a filled order's.
//! * A header line whose date range **lies** (`since 8/27/26 through 8/27/26`
//!   in a file whose rows are all 8/28), so `as_of` comes from the row dates
//!   cross-checked against the file name.
//! * All times are Central (`America/Chicago` by default).
//!
//! Anything that is not an option row — equities, futures, forex, crypto —
//! is dropped.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use csv::StringRecord;
use rust_decimal::Decimal;
use serde::Serialize;

use crate::contract::{OptionContract, Right};
use crate::dates::{parse_expiry, parse_time, parse_us_date, parse_us_datetime};
use crate::error::ParseError;
use crate::order::{Execution, Fees, Fill, Order, PosEffect, Side};
use crate::strategy::Strategy;
use crate::tos_order;
use crate::warning::Warning;

/// Statements are exported in the account's local (Central) time.
pub const DEFAULT_TIMEZONE: Tz = chrono_tz::America::Chicago;
/// Statements never print the multiplier; SPX options are 100.
const DEFAULT_MULTIPLIER: u32 = 100;

/// One `TRD` row of the `Cash Balance` section that is an option trade.
///
/// Non-option rows (equity/ETF rebalances) are dropped: the description is
/// run through [`tos_order::parse`] and only what parses as an option order
/// is kept, which is both the filter and the parse.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CashTrade {
    pub at: DateTime<Tz>,
    /// `="9000000000032"` with the Excel wrapper removed.
    pub ref_no: Option<String>,
    pub description: String,
    /// The description parsed as an order (that is what makes it an option row).
    pub order: Order,
    /// Positive cost.
    pub fees: Fees,
    /// Signed cash flow: negative for a debit.
    pub amount: Decimal,
    pub balance: Option<Decimal>,
}

/// What became of an order, from *Account Order History*.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderState {
    Filled,
    Canceled,
    Working,
    Rejected,
    Expired,
    Replaced,
    Other(String),
}

impl OrderState {
    fn from_token(token: &str) -> Self {
        match token.trim().to_ascii_uppercase().as_str() {
            "FILLED" => OrderState::Filled,
            "CANCELED" | "CANCELLED" => OrderState::Canceled,
            "WORKING" | "QUEUED" | "OPEN" => OrderState::Working,
            "REJECTED" => OrderState::Rejected,
            "EXPIRED" => OrderState::Expired,
            "REPLACED" => OrderState::Replaced,
            other => OrderState::Other(other.to_string()),
        }
    }
}

/// One leg of an order-history row. There is no OPRA code in this section, so
/// the settlement is unknowable here and no contract is built — the fields
/// stay raw on purpose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrderStatusLeg {
    pub side: Side,
    pub qty: i32,
    pub pos_effect: Option<PosEffect>,
    pub symbol: String,
    pub expiry: NaiveDate,
    pub strike: Decimal,
    pub right: Right,
}

/// One order as the broker saw it: what was asked for, and what happened.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OrderStatusRow {
    pub placed_at: DateTime<Tz>,
    pub strategy: Strategy,
    pub state: OrderState,
    /// The *limit* price — this is not the fill price and differs from it
    /// often enough to matter.
    pub limit_price: Option<Decimal>,
    pub order_type: Option<String>,
    pub tif: Option<String>,
    /// `RE #9000000000008` from a continuation row: the order this one
    /// replaced.
    pub replaces_ref: Option<String>,
    pub legs: Vec<OrderStatusLeg>,
}

/// A row of the account-level `Options` position section.
///
/// This section is netted **across trades**, so `trade_price` is a blended
/// average and is deliberately ignored by reconciliation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OptionPosition {
    pub contract: OptionContract,
    pub qty: i32,
    /// Blended cost basis across every trade — do not attribute it.
    pub trade_price: Option<Decimal>,
    pub mark: Option<Decimal>,
    pub mark_value: Option<Decimal>,
}

/// The `Account Summary` key/value block. Values that are `N/A` are `None`.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct AccountSummary {
    pub entries: BTreeMap<String, Option<Decimal>>,
}

impl AccountSummary {
    #[must_use]
    pub fn get(&self, key: &str) -> Option<Decimal> {
        self.entries
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .and_then(|(_, v)| *v)
    }

    #[must_use]
    pub fn net_liquidating_value(&self) -> Option<Decimal> {
        self.get("Net Liquidating Value")
    }
}

/// One parsed statement.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Statement {
    pub path: PathBuf,
    /// The day the rows are about. From the row dates, cross-checked against
    /// the file name — **never** from the header line, which lies.
    pub as_of: NaiveDate,
    pub timezone: Tz,
    /// The header line's `since … through …` range, kept only as a
    /// diagnostic. The account number on that line is deliberately not read.
    pub header_range: Option<(NaiveDate, NaiveDate)>,
    pub cash_trades: Vec<CashTrade>,
    pub executed_orders: Vec<Execution>,
    pub order_status: Vec<OrderStatusRow>,
    pub option_positions: Vec<OptionPosition>,
    pub account_summary: AccountSummary,
    pub warnings: Vec<Warning>,
}

/// Parse a statement with the default (Central) timezone.
pub fn parse(path: &Path) -> Result<Statement, ParseError> {
    parse_with_tz(path, DEFAULT_TIMEZONE)
}

/// Parse a statement, reading its times in `tz`.
pub fn parse_with_tz(path: &Path, tz: Tz) -> Result<Statement, ParseError> {
    let bytes = fs::read(path)
        .map_err(|e| ParseError::new(format!("cannot read statement: {e}")).at(path, 0))?;
    let text = String::from_utf8_lossy(&bytes);
    // `from_utf8_lossy` only allocates when it had to replace something, which
    // is the one signal that the download is corrupt.
    let lossy = matches!(text, std::borrow::Cow::Owned(_));
    let mut statement = parse_str(&text, path, tz)?;
    if lossy {
        statement.warnings.insert(
            0,
            Warning::NonUtf8File {
                path: path.to_path_buf(),
            },
        );
    }
    Ok(statement)
}

/// Parse statement text that has already been read (BOM and CRLF tolerated).
pub fn parse_str(text: &str, path: &Path, tz: Tz) -> Result<Statement, ParseError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .has_headers(false)
        .from_reader(text.as_bytes());

    let mut warnings = Vec::new();
    let mut header_range = None;
    let mut section: Option<Kind> = None;
    let mut expect_columns = false;

    let mut cash_rows: Vec<(u64, StringRecord)> = Vec::new();
    let mut trade_rows: Vec<(u64, StringRecord)> = Vec::new();
    let mut order_rows: Vec<(u64, StringRecord)> = Vec::new();
    let mut option_rows: Vec<(u64, StringRecord)> = Vec::new();
    let mut summary_rows: Vec<(u64, StringRecord)> = Vec::new();
    let mut cash_columns = Columns::default();
    let mut trade_columns = Columns::default();
    let mut order_columns = Columns::default();
    let mut option_columns = Columns::default();
    let mut seen_any_row = false;

    for result in reader.records() {
        let record = match result {
            Ok(r) => r,
            Err(e) => {
                warnings.push(Warning::StatementRowSkipped {
                    path: path.to_path_buf(),
                    section: section.as_ref().map_or("preamble", Kind::name).to_string(),
                    line: 0,
                    reason: e.to_string(),
                });
                continue;
            }
        };
        let line = record.position().map_or(0, csv::Position::line);

        if !seen_any_row && section.is_none() {
            if let Some(range) = parse_header_range(record.get(0).unwrap_or_default()) {
                header_range = Some(range);
            }
        }
        seen_any_row = true;

        if let Some(header) = section_header(&record) {
            if let Kind::Ignored(name) = &header {
                warnings.push(Warning::StatementSectionSkipped {
                    path: path.to_path_buf(),
                    section: name.clone(),
                });
            }
            expect_columns = header.has_column_header();
            section = Some(header);
            continue;
        }
        if expect_columns {
            // The lone-space line moves between sections from file to file; if
            // it lands right after a title it must not be taken for the header,
            // or every lookup in the section returns "" and the whole section
            // vanishes.
            if record.iter().all(|f| f.trim().is_empty()) {
                continue;
            }
            match section {
                Some(Kind::CashBalance) => cash_columns = Columns::new(&record),
                Some(Kind::TradeHistory) => trade_columns = Columns::new(&record),
                Some(Kind::OrderHistory) => order_columns = Columns::new(&record),
                Some(Kind::Options) => option_columns = Columns::new(&record),
                _ => {}
            }
            expect_columns = false;
            continue;
        }

        match section {
            Some(Kind::CashBalance) => cash_rows.push((line, record)),
            Some(Kind::TradeHistory) => trade_rows.push((line, record)),
            Some(Kind::OrderHistory) => order_rows.push((line, record)),
            Some(Kind::Options) => option_rows.push((line, record)),
            Some(Kind::AccountSummary) => summary_rows.push((line, record)),
            _ => {}
        }
    }

    let mut ctx = Ctx {
        path: path.to_path_buf(),
        tz,
        warnings,
    };
    for (kind, columns, rows) in [
        (Kind::CashBalance, &cash_columns, &cash_rows),
        (Kind::TradeHistory, &trade_columns, &trade_rows),
        (Kind::OrderHistory, &order_columns, &order_rows),
        (Kind::Options, &option_columns, &option_rows),
    ] {
        check_required_columns(&kind, columns, rows, &mut ctx);
    }
    let cash_trades = parse_cash(&cash_rows, &cash_columns, &mut ctx);
    let mut executed_orders = parse_trade_history(&trade_rows, &trade_columns, &mut ctx);
    let order_status = parse_order_history(&order_rows, &order_columns, &mut ctx);
    let option_positions = parse_options(&option_rows, &option_columns, &mut ctx);
    let account_summary = parse_summary(&summary_rows);

    link_cash_to_executions(&cash_trades, &mut executed_orders, &mut ctx);
    link_pos_effects(&order_status, &mut executed_orders);

    let as_of = resolve_as_of(
        path,
        &cash_trades,
        &executed_orders,
        &order_status,
        &mut ctx.warnings,
    )?;

    Ok(Statement {
        path: path.to_path_buf(),
        as_of,
        timezone: tz,
        header_range,
        cash_trades,
        executed_orders,
        order_status,
        option_positions,
        account_summary,
        warnings: ctx.warnings,
    })
}

/// Parse every `*.csv` in a statements directory, oldest first.
///
/// Exactly one statement survives per day: a re-downloaded export
/// (`… (1).csv`) would otherwise hand the assembler a second copy of every
/// execution, which its single-claim rule reads as a genuine duplicate.
#[must_use]
pub fn load_dir(dir: &Path, tz: Tz) -> (Vec<Statement>, Vec<Warning>) {
    let mut statements = Vec::new();
    let mut warnings = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            warnings.push(Warning::StatementParseFailed {
                path: dir.to_path_buf(),
                message: format!("cannot read directory: {e}"),
            });
            return (statements, warnings);
        }
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.is_file()
                && !p
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
                && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("csv"))
        })
        .collect();
    paths.sort();
    for path in paths {
        match parse_with_tz(&path, tz) {
            Ok(statement) => statements.push(statement),
            Err(e) => warnings.push(Warning::StatementParseFailed {
                path: path.clone(),
                message: e.message,
            }),
        }
    }
    statements.sort_by_key(|s| s.as_of);
    // Drain the per-file warnings only after de-duplication, so that an
    // ignored copy does not report everything twice.
    let mut statements = dedupe_by_date(statements, &mut warnings);
    for statement in &mut statements {
        warnings.append(&mut statement.warnings);
    }
    (statements, warnings)
}

/// Keep one statement per day — the most recently modified export, ties going
/// to the later file name — and report every copy dropped.
fn dedupe_by_date(statements: Vec<Statement>, warnings: &mut Vec<Warning>) -> Vec<Statement> {
    let mut kept: Vec<Statement> = Vec::with_capacity(statements.len());
    for statement in statements {
        let Some(previous) = kept.last() else {
            kept.push(statement);
            continue;
        };
        if previous.as_of != statement.as_of {
            kept.push(statement);
            continue;
        }
        let (winner, loser) = if modified_at(&statement.path) >= modified_at(&previous.path) {
            (statement, kept.pop().expect("just checked"))
        } else {
            (kept.pop().expect("just checked"), statement)
        };
        warnings.push(Warning::DuplicateStatement {
            path: loser.path,
            as_of: winner.as_of,
            kept: winner.path.clone(),
        });
        kept.push(winner);
    }
    kept
}

fn modified_at(path: &Path) -> Option<std::time::SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

// ---------------------------------------------------------------- sections

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    CashBalance,
    OrderHistory,
    TradeHistory,
    Options,
    AccountSummary,
    /// A section we know about but do not model, or one we have never seen.
    Ignored(String),
}

impl Kind {
    fn name(&self) -> &str {
        match self {
            Kind::CashBalance => "Cash Balance",
            Kind::OrderHistory => "Account Order History",
            Kind::TradeHistory => "Account Trade History",
            Kind::Options => "Options",
            Kind::AccountSummary => "Account Summary",
            Kind::Ignored(n) => n,
        }
    }

    /// `Account Summary` is the odd one out: it is key/value rows with no
    /// column header.
    fn has_column_header(&self) -> bool {
        !matches!(self, Kind::AccountSummary)
    }
}

/// Is this record a section title?
///
/// A title is a row with exactly one non-empty field. Known names win; a
/// single field that still looks like a title (no digits, no `$`) is treated
/// as an unknown section so a future TOS section is skipped loudly rather
/// than silently mangled. `"Total Cash $79,984.05"` and the lone-space line
/// are both correctly *not* titles.
fn section_header(record: &StringRecord) -> Option<Kind> {
    let mut non_empty = record.iter().map(str::trim).filter(|f| !f.is_empty());
    let text = non_empty.next()?;
    if non_empty.next().is_some() {
        return None;
    }
    for (name, kind) in [
        ("Cash Balance", Kind::CashBalance),
        ("Account Order History", Kind::OrderHistory),
        ("Account Trade History", Kind::TradeHistory),
        ("Options", Kind::Options),
        ("Account Summary", Kind::AccountSummary),
    ] {
        if text.eq_ignore_ascii_case(name) {
            return Some(kind);
        }
    }
    for name in [
        "Futures Statements",
        "Forex Statements",
        "Equities",
        "Profits and Losses",
    ] {
        if text.eq_ignore_ascii_case(name) {
            return Some(Kind::Ignored(name.to_string()));
        }
    }
    if text.starts_with("Crypto") && text.ends_with("Statements") {
        return Some(Kind::Ignored("Crypto Statements".to_string()));
    }
    let looks_like_a_title = text.len() <= 64
        && !text.contains('$')
        && !text.bytes().any(|b| b.is_ascii_digit())
        && text.starts_with(|c: char| c.is_ascii_uppercase());
    looks_like_a_title.then(|| Kind::Ignored(text.to_string()))
}

/// `Account Statement for … since 8/18/26 through 8/18/26`. The account
/// number on this line is intentionally never read.
fn parse_header_range(line: &str) -> Option<(NaiveDate, NaiveDate)> {
    let rest = line.split(" since ").nth(1)?;
    let (from, to) = rest.split_once(" through ")?;
    Some((
        parse_us_date(from.trim())?,
        parse_us_date(to.trim().trim_end_matches('"'))?,
    ))
}

// ----------------------------------------------------------------- columns

#[derive(Debug, Clone, Default)]
struct Columns {
    names: Vec<String>,
}

impl Columns {
    fn new(record: &StringRecord) -> Self {
        Self {
            names: record.iter().map(|f| f.trim().to_string()).collect(),
        }
    }

    fn index(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n.eq_ignore_ascii_case(name))
    }

    /// The named field, or `""` when the column or the cell is missing.
    fn field<'a>(&self, record: &'a StringRecord, name: &str) -> &'a str {
        self.index(name)
            .and_then(|i| record.get(i))
            .unwrap_or_default()
            .trim()
    }

    fn at(record: &StringRecord, index: usize) -> &str {
        record.get(index).unwrap_or_default().trim()
    }
}

/// The columns a section cannot be read without.
///
/// Everything is looked up by name so that column *drift* is survivable, but a
/// rename of one of these turns every row into "not an option row" and empties
/// the section. Check them once, up front, rather than discovering it row by
/// row and then not reporting it.
fn required_columns(kind: &Kind) -> &'static [&'static str] {
    match kind {
        Kind::CashBalance => &["DATE", "TIME", "TYPE", "DESCRIPTION"],
        Kind::TradeHistory => &[
            "Exec Time",
            "Spread",
            "Qty",
            "Price",
            "Symbol",
            "Type",
            "Opra",
        ],
        Kind::OrderHistory => &[
            "Time Placed",
            "Spread",
            "Side",
            "Qty",
            "Exp",
            "Strike",
            "Type",
            "Status",
        ],
        Kind::Options => &["Symbol", "Option Code", "Qty"],
        Kind::AccountSummary | Kind::Ignored(_) => &[],
    }
}

fn check_required_columns(
    kind: &Kind,
    columns: &Columns,
    rows: &[(u64, StringRecord)],
    ctx: &mut Ctx,
) {
    if rows.is_empty() {
        return;
    }
    for name in required_columns(kind) {
        if columns.index(name).is_none() {
            ctx.warnings.push(Warning::StatementColumnMissing {
                path: ctx.path.clone(),
                section: kind.name().to_string(),
                column: (*name).to_string(),
            });
        }
    }
}

struct Ctx {
    path: PathBuf,
    tz: Tz,
    warnings: Vec<Warning>,
}

impl Ctx {
    fn skip(&mut self, section: &str, line: u64, reason: impl Into<String>) {
        self.warnings.push(Warning::StatementRowSkipped {
            path: self.path.clone(),
            section: section.to_string(),
            line,
            reason: reason.into(),
        });
    }

    /// Local -> zoned, tolerating the two hours a year that are ambiguous.
    fn zoned(&self, naive: NaiveDateTime) -> Option<DateTime<Tz>> {
        match self.tz.from_local_datetime(&naive) {
            // Twice a year a local time happens twice; take the first.
            chrono::LocalResult::Single(at) | chrono::LocalResult::Ambiguous(at, _) => Some(at),
            chrono::LocalResult::None => None,
        }
    }
}

// -------------------------------------------------------------- numerics

/// Strip Excel's `="…"` wrapper and any stray surrounding quotes.
#[must_use]
pub fn clean_str(raw: &str) -> &str {
    let s = raw.trim();
    let s = s.strip_prefix('=').unwrap_or(s);
    let s = s.strip_prefix('"').unwrap_or(s);
    let s = s.strip_suffix('"').unwrap_or(s);
    s.trim()
}

/// The statement numeric cleaner: `$`, thousands separators, parenthesised
/// negatives, leading-dot decimals, `="…"`, and the `--`/`N/A`/`~` sentinels.
#[must_use]
pub fn clean_decimal(raw: &str) -> Option<Decimal> {
    let mut s = clean_str(raw).to_string();
    if s.is_empty() {
        return None;
    }
    if matches!(
        s.to_ascii_uppercase().as_str(),
        "--" | "-" | "N/A" | "NA" | "~" | "DEBIT" | "CREDIT" | "NONE"
    ) {
        return None;
    }
    let negative = s.starts_with('(') && s.ends_with(')');
    if negative {
        s = s[1..s.len() - 1].to_string();
    }
    s.retain(|c| !matches!(c, '$' | '"'));
    let mut s = s.trim().to_string();
    if let Some(rest) = s.strip_prefix('+') {
        // `+-5` is not a number; only a lone leading `+` is decoration.
        if rest.starts_with(['+', '-']) {
            return None;
        }
        s = rest.to_string();
    }
    if let Some(rest) = s.strip_prefix('.') {
        s = format!("0.{rest}");
    } else if let Some(rest) = s.strip_prefix("-.") {
        s = format!("-0.{rest}");
    }
    // `($9,735.00)` is how TOS writes a debit; `(-5)` is junk, not a credit.
    if negative && s.starts_with('-') {
        return None;
    }
    let value = plain_decimal(&s)?;
    Some(if negative { -value } else { value })
}

/// The shapes a statement money cell actually takes: an optional `-`, digits
/// in optional `,` thousands groups, and an optional fractional part.
///
/// `Decimal::from_str` is far looser than the statements ever are (`1e3`,
/// `1,2,3` and `1 000` all parse), and a junk cell silently coerced into money
/// is worse than a `None` that skips the row with a warning.
fn plain_decimal(s: &str) -> Option<Decimal> {
    let body = s.strip_prefix('-').unwrap_or(s);
    let (int, frac) = match body.split_once('.') {
        // A bare `.` and a trailing `5.` are not numbers TOS writes.
        Some((_, "")) => return None,
        Some(split) => split,
        None => (body, ""),
    };
    if (int.is_empty() && frac.is_empty()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let groups: Vec<&str> = int.split(',').collect();
    if !groups.iter().all(|g| g.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    if groups.len() > 1
        && !(groups[0].len() <= 3
            && !groups[0].is_empty()
            && groups[1..].iter().all(|g| g.len() == 3))
    {
        return None;
    }
    let mut cleaned = s.to_string();
    cleaned.retain(|c| c != ',');
    cleaned.parse().ok()
}

fn clean_qty(raw: &str) -> Option<i32> {
    use rust_decimal::prelude::ToPrimitive;
    clean_decimal(raw).and_then(|d| d.trunc().to_i32())
}

// -------------------------------------------------------------- sections

fn parse_cash(rows: &[(u64, StringRecord)], columns: &Columns, ctx: &mut Ctx) -> Vec<CashTrade> {
    const SECTION: &str = "Cash Balance";
    let mut out = Vec::new();
    for (line, record) in rows {
        let kind = columns.field(record, "TYPE").to_ascii_uppercase();
        if kind != "TRD" {
            continue;
        }
        let description = columns.field(record, "DESCRIPTION").to_string();
        // `parse_with_warnings`, not `parse`: a description without an
        // `[AM]`/`(Weeklys)` token gets its root and settlement from the
        // per-underlying default, and that guess is the difference between
        // `SPX…` (AM, 09:30 ET) and `SPXW…` (PM, 16:00 ET). Dropping the
        // warning would make the guess invisible.
        let order = match tos_order::parse_with_warnings(&description) {
            Ok((order, mut order_warnings)) => {
                ctx.warnings.append(&mut order_warnings);
                order
            }
            Err(e) => {
                // Equity/ETF rows land here by design and are dropped
                // silently; anything that *looks* like an option row but does
                // not parse is worth complaining about.
                if looks_like_option_description(&description) {
                    ctx.skip(
                        SECTION,
                        *line,
                        format!("option-shaped row did not parse: {e}"),
                    );
                }
                continue;
            }
        };
        let Some(date) = parse_us_date(columns.field(record, "DATE")) else {
            ctx.skip(SECTION, *line, "unparseable DATE");
            continue;
        };
        let Some(time) = parse_time(columns.field(record, "TIME")) else {
            ctx.skip(SECTION, *line, "unparseable TIME");
            continue;
        };
        let Some(at) = ctx.zoned(date.and_time(time)) else {
            ctx.skip(SECTION, *line, "timestamp does not exist in this timezone");
            continue;
        };
        let ref_no = {
            let r = clean_str(columns.field(record, "REF #"));
            (!r.is_empty()).then(|| r.to_string())
        };
        let amount = clean_decimal(columns.field(record, "AMOUNT")).unwrap_or_default();
        let fees = Fees {
            commission: clean_decimal(columns.field(record, "Commissions & Fees"))
                .unwrap_or_default()
                .abs(),
            misc: clean_decimal(columns.field(record, "Misc Fees"))
                .unwrap_or_default()
                .abs(),
        };
        out.push(CashTrade {
            at,
            ref_no,
            description,
            order,
            fees,
            amount,
            balance: clean_decimal(columns.field(record, "BALANCE")),
        });
    }
    out
}

/// A cash description that mentions a contract multiplier and a price is an
/// option trade; `BOT +1 DUMMY @127.11` is not.
fn looks_like_option_description(description: &str) -> bool {
    description.contains(" 100 ") && description.contains('@')
}

/// Group rows into spreads: a non-empty `key` column starts a new group.
fn group_rows<'a>(
    rows: &'a [(u64, StringRecord)],
    columns: &Columns,
    key: &str,
) -> Vec<Vec<&'a (u64, StringRecord)>> {
    let mut groups: Vec<Vec<&(u64, StringRecord)>> = Vec::new();
    for row in rows {
        if columns.field(&row.1, key).is_empty() {
            if let Some(last) = groups.last_mut() {
                last.push(row);
                continue;
            }
            // A continuation row with nothing to continue: ignore it.
            continue;
        }
        groups.push(vec![row]);
    }
    groups
}

fn parse_trade_history(
    rows: &[(u64, StringRecord)],
    columns: &Columns,
    ctx: &mut Ctx,
) -> Vec<Execution> {
    const SECTION: &str = "Account Trade History";
    let mut out = Vec::new();
    for group in group_rows(rows, columns, "Exec Time") {
        let (line, first) = (group[0].0, &group[0].1);
        // Non-option groups (stock/ETF, `Opra` = `N/A`) are simply not ours.
        if group.iter().any(|(_, r)| {
            Right::from_token(columns.field(r, "Type")).is_none()
                || clean_str(columns.field(r, "Opra")).is_empty()
                || columns.field(r, "Opra").eq_ignore_ascii_case("N/A")
        }) {
            continue;
        }
        let Some(naive) = parse_us_datetime(columns.field(first, "Exec Time")) else {
            ctx.skip(SECTION, line, "unparseable Exec Time");
            continue;
        };
        let Some(at) = ctx.zoned(naive) else {
            ctx.skip(SECTION, line, "timestamp does not exist in this timezone");
            continue;
        };
        let spread = columns.field(first, "Spread");
        let Some(strategy) = Strategy::from_token(spread) else {
            ctx.skip(SECTION, line, format!("unknown spread {spread:?}"));
            continue;
        };
        let mut fills = Vec::with_capacity(group.len());
        let mut bad = false;
        for (row_line, record) in &group {
            let symbol = columns.field(record, "Symbol");
            let code = clean_str(columns.field(record, "Opra"));
            let contract = match OptionContract::from_tos_code(code, symbol, DEFAULT_MULTIPLIER) {
                Ok(c) => c,
                Err(e) => {
                    ctx.skip(SECTION, *row_line, format!("{code}: {e}"));
                    bad = true;
                    break;
                }
            };
            // The OPRA code is authoritative; the Exp/Strike columns are a
            // cross-check (they carry no settlement flag, so they cannot
            // contradict the root).
            if let Ok(expiry) = parse_expiry(columns.field(record, "Exp")) {
                let strike = clean_decimal(columns.field(record, "Strike"));
                let right = Right::from_token(columns.field(record, "Type"));
                if expiry.date != contract.expiry
                    || strike.is_some_and(|s| s != contract.strike)
                    || right.is_some_and(|r| r != contract.right)
                {
                    ctx.skip(
                        SECTION,
                        *row_line,
                        format!("row disagrees with its OPRA code {code}; using the code"),
                    );
                }
            }
            let Some(qty) = clean_qty(columns.field(record, "Qty")) else {
                ctx.skip(SECTION, *row_line, "unparseable Qty");
                bad = true;
                break;
            };
            let Some(price) = clean_decimal(columns.field(record, "Price")) else {
                ctx.skip(SECTION, *row_line, "unparseable Price");
                bad = true;
                break;
            };
            fills.push(Fill {
                contract,
                qty,
                price,
                pos_effect: None,
            });
        }
        if bad || fills.is_empty() {
            continue;
        }
        out.push(Execution {
            at,
            ref_no: None,
            strategy,
            net_price: clean_decimal(columns.field(first, "Net Price")),
            fills,
            fees: Fees::default(),
            amount: None,
            source: ctx.path.clone(),
        });
    }
    out
}

fn parse_order_history(
    rows: &[(u64, StringRecord)],
    columns: &Columns,
    ctx: &mut Ctx,
) -> Vec<OrderStatusRow> {
    const SECTION: &str = "Account Order History";
    // The section has two unnamed columns; the order type is the one just
    // before TIF. Look everything else up by name so column drift is
    // survivable.
    let order_type_index = columns.index("TIF").and_then(|i| i.checked_sub(1));
    let mut out = Vec::new();
    for group in group_rows(rows, columns, "Time Placed") {
        let (line, first) = (group[0].0, &group[0].1);
        if group
            .iter()
            .any(|(_, r)| Right::from_token(columns.field(r, "Type")).is_none())
        {
            continue; // stock/ETF order
        }
        let Some(naive) = parse_us_datetime(columns.field(first, "Time Placed")) else {
            ctx.skip(SECTION, line, "unparseable Time Placed");
            continue;
        };
        let Some(placed_at) = ctx.zoned(naive) else {
            ctx.skip(SECTION, line, "timestamp does not exist in this timezone");
            continue;
        };
        let spread = columns.field(first, "Spread");
        let Some(strategy) = Strategy::from_token(spread) else {
            ctx.skip(SECTION, line, format!("unknown spread {spread:?}"));
            continue;
        };

        let mut replaces_ref = None;
        let mut legs = Vec::with_capacity(group.len());
        let mut bad = false;
        for (row_line, record) in &group {
            // A replace-order reference squats in the continuation row's
            // Spread column: `,,,RE #9000000000008,SELL,…`.
            let spread_cell = columns.field(record, "Spread");
            if let Some(reference) = spread_cell.strip_prefix("RE #") {
                replaces_ref = Some(reference.trim().to_string());
            }
            let Some(side) = Side::from_token(columns.field(record, "Side")) else {
                ctx.skip(SECTION, *row_line, "unparseable Side");
                bad = true;
                break;
            };
            let Some(qty) = clean_qty(columns.field(record, "Qty")) else {
                ctx.skip(SECTION, *row_line, "unparseable Qty");
                bad = true;
                break;
            };
            let expiry = match parse_expiry(columns.field(record, "Exp")) {
                Ok(e) => e.date,
                Err(e) => {
                    ctx.skip(SECTION, *row_line, e.message);
                    bad = true;
                    break;
                }
            };
            let Some(strike) = clean_decimal(columns.field(record, "Strike")) else {
                ctx.skip(SECTION, *row_line, "unparseable Strike");
                bad = true;
                break;
            };
            let Some(right) = Right::from_token(columns.field(record, "Type")) else {
                ctx.skip(SECTION, *row_line, "unparseable Type");
                bad = true;
                break;
            };
            legs.push(OrderStatusLeg {
                side,
                qty,
                pos_effect: PosEffect::from_token(columns.field(record, "Pos Effect")),
                symbol: columns.field(record, "Symbol").to_string(),
                expiry,
                strike,
                right,
            });
        }
        if bad || legs.is_empty() {
            continue;
        }
        out.push(OrderStatusRow {
            placed_at,
            strategy,
            state: OrderState::from_token(columns.field(first, "Status")),
            limit_price: clean_decimal(columns.field(first, "PRICE")),
            order_type: order_type_index
                .map(|i| Columns::at(first, i))
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            tif: {
                let t = columns.field(first, "TIF");
                (!t.is_empty()).then(|| t.to_string())
            },
            replaces_ref,
            legs,
        });
    }
    out
}

fn parse_options(
    rows: &[(u64, StringRecord)],
    columns: &Columns,
    ctx: &mut Ctx,
) -> Vec<OptionPosition> {
    const SECTION: &str = "Options";
    let mut out = Vec::new();
    for (line, record) in rows {
        let code = clean_str(columns.field(record, "Option Code"));
        let symbol = columns.field(record, "Symbol");
        // `,OVERALL TOTALS,,,,,,,"$54,150.00"` closes the section.
        if symbol.is_empty() || code.is_empty() || code.eq_ignore_ascii_case("OVERALL TOTALS") {
            continue;
        }
        let contract = match OptionContract::from_tos_code(code, symbol, DEFAULT_MULTIPLIER) {
            Ok(c) => c,
            Err(e) => {
                ctx.skip(SECTION, *line, format!("{code}: {e}"));
                continue;
            }
        };
        let Some(qty) = clean_qty(columns.field(record, "Qty")) else {
            ctx.skip(SECTION, *line, "unparseable Qty");
            continue;
        };
        out.push(OptionPosition {
            contract,
            qty,
            trade_price: clean_decimal(columns.field(record, "Trade Price")),
            mark: clean_decimal(columns.field(record, "Mark")),
            mark_value: clean_decimal(columns.field(record, "Mark Value")),
        });
    }
    out
}

fn parse_summary(rows: &[(u64, StringRecord)]) -> AccountSummary {
    let mut entries = BTreeMap::new();
    for (_, record) in rows {
        let key = Columns::at(record, 0);
        if key.is_empty() {
            continue;
        }
        entries.insert(key.to_string(), clean_decimal(Columns::at(record, 1)));
    }
    AccountSummary { entries }
}

// -------------------------------------------------------------- linking

/// Attach the `Cash Balance` reference, fees and cash amount to the execution
/// they belong to.
///
/// Matching is on the netted legs rather than on the clock: the two sections
/// disagree by a second often enough (2026-08-04) that time can only be a
/// tie-breaker.
///
/// The key deliberately drops root and settlement: the Cash Balance side has
/// no OPRA code, so those two fields are `resolve_root`'s guess, and including
/// them would fail the link on exactly the descriptions that lack an
/// `[AM]`/`(Weeklys)` token (the same reasoning as the assembler's `LegKey`).
/// Net price is a tie-breaker for the same reason — this is the only source of
/// `Execution::fees`, and a failed link puts a zero into P&L rather than a
/// number, so it is warned about at both ends.
fn link_cash_to_executions(cash: &[CashTrade], executions: &mut [Execution], ctx: &mut Ctx) {
    let cash_keys: Vec<Vec<LegKey>> = cash
        .iter()
        .map(|c| leg_keys(&c.order.canonical_legs()))
        .collect();
    let mut used = vec![false; cash.len()];
    for execution in &mut *executions {
        let key = leg_keys(&execution.canonical_legs());
        let best = cash
            .iter()
            .enumerate()
            .filter(|(i, _)| !used[*i] && cash_keys[*i] == key)
            .min_by_key(|(_, c)| {
                (
                    execution.net_price.is_some_and(|p| p != c.order.net_price),
                    (c.at - execution.at).num_seconds().abs(),
                )
            });
        if let Some((index, trade)) = best {
            used[index] = true;
            execution.ref_no.clone_from(&trade.ref_no);
            execution.fees = trade.fees;
            execution.amount = Some(trade.amount);
        } else {
            ctx.warnings.push(Warning::ExecutionFeesUnlinked {
                path: ctx.path.clone(),
                at: execution.at.date_naive(),
                contracts: execution
                    .fills
                    .iter()
                    .map(|f| f.contract.to_tos_code())
                    .collect(),
            });
        }
    }
    for (index, trade) in cash.iter().enumerate() {
        if !used[index] {
            ctx.warnings.push(Warning::CashTradeUnlinked {
                path: ctx.path.clone(),
                description: trade.description.clone(),
            });
        }
    }
}

/// A leg as both statement sections can agree on it.
type LegKey = (NaiveDate, Decimal, Right, i32);

fn leg_keys(legs: &[(OptionContract, i32)]) -> Vec<LegKey> {
    let mut keys: Vec<LegKey> = legs
        .iter()
        .map(|(c, qty)| (c.expiry, c.strike, c.right, *qty))
        .collect();
    keys.sort();
    keys
}

/// Copy `TO OPEN`/`TO CLOSE` from the *filled* order-history group onto the
/// execution's legs. Only `Filled` groups are considered, which is what keeps
/// a `CANCELED` order with identical legs from contributing anything.
fn link_pos_effects(order_status: &[OrderStatusRow], executions: &mut [Execution]) {
    for execution in &mut *executions {
        let candidates: Vec<&OrderStatusRow> = order_status
            .iter()
            .filter(|row| {
                row.state == OrderState::Filled
                    && row.strategy == execution.strategy
                    && row.legs.len() == execution.fills.len()
                    && row.legs.iter().all(|leg| {
                        execution.fills.iter().any(|fill| {
                            fill.contract.expiry == leg.expiry
                                && fill.contract.strike == leg.strike
                                && fill.contract.right == leg.right
                                && fill.qty == leg.qty
                        })
                    })
            })
            .collect();
        // No match, or an ambiguous one: leave the effects unknown.
        let [row] = candidates[..] else { continue };
        for fill in &mut execution.fills {
            if let Some(leg) = row.legs.iter().find(|leg| {
                fill.contract.expiry == leg.expiry
                    && fill.contract.strike == leg.strike
                    && fill.contract.right == leg.right
                    && fill.qty == leg.qty
            }) {
                fill.pos_effect = leg.pos_effect;
            }
        }
    }
}

/// `as_of` from the row dates, cross-checked against the file name.
///
/// The header line's range is never consulted: it is wrong in every file we
/// have. Only rows that are evidence of *this* statement day vote: a working
/// or canceled GTC order carries the day it was originally entered, and a
/// handful of those would otherwise outvote the day's own fills and drag the
/// whole book's valuation date backwards.
fn resolve_as_of(
    path: &Path,
    cash: &[CashTrade],
    executions: &[Execution],
    orders: &[OrderStatusRow],
    warnings: &mut Vec<Warning>,
) -> Result<NaiveDate, ParseError> {
    let mut counts: BTreeMap<NaiveDate, usize> = BTreeMap::new();
    for date in cash
        .iter()
        .map(|c| c.at.date_naive())
        .chain(executions.iter().map(|e| e.at.date_naive()))
        .chain(
            orders
                .iter()
                .filter(|o| o.state == OrderState::Filled)
                .map(|o| o.placed_at.date_naive()),
        )
    {
        *counts.entry(date).or_default() += 1;
    }
    // Most rows win; the latest date breaks a tie.
    let row_date = counts
        .iter()
        .max_by_key(|(date, count)| (**count, **date))
        .map(|(date, _)| *date);
    let file_date = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.get(..10))
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());

    match (row_date, file_date) {
        (Some(row), Some(file)) => {
            if row != file {
                warnings.push(Warning::StatementDateDisagreement {
                    path: path.to_path_buf(),
                    file_date: file,
                    row_date: row,
                });
            }
            Ok(row)
        }
        (Some(row), None) => Ok(row),
        (None, Some(file)) => Ok(file),
        (None, None) => Err(ParseError::new(
            "statement has no dated rows and no date in its file name",
        )
        .at(path, 0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn numeric_cleaner_handles_every_tos_spelling() {
        assert_eq!(clean_decimal("$110,295.01"), Some(dec!(110295.01)));
        assert_eq!(clean_decimal("\"1,234.56\""), Some(dec!(1234.56)));
        assert_eq!(clean_decimal("($9,735.00)"), Some(dec!(-9735.00)));
        assert_eq!(clean_decimal("(123.45)"), Some(dec!(-123.45)));
        assert_eq!(clean_decimal(".725"), Some(dec!(0.725)));
        assert_eq!(clean_decimal(".10"), Some(dec!(0.10)));
        assert_eq!(clean_decimal("-.57"), Some(dec!(-0.57)));
        assert_eq!(clean_decimal("+2"), Some(dec!(2)));
        assert_eq!(clean_decimal("-4"), Some(dec!(-4)));
        assert_eq!(clean_decimal("--"), None);
        assert_eq!(clean_decimal("N/A"), None);
        assert_eq!(clean_decimal("~"), None);
        assert_eq!(clean_decimal("DEBIT"), None);
        assert_eq!(clean_decimal("CREDIT"), None);
        assert_eq!(clean_decimal(""), None);
        assert_eq!(clean_decimal("banana"), None);
        assert_eq!(
            clean_decimal("=\"9000000000001\""),
            Some(dec!(9000000000001))
        );
    }

    /// The cleaner is tolerant of TOS's spellings and nothing else: a junk
    /// cell coerced into money is worse than a `None` that skips the row.
    #[test]
    fn the_numeric_cleaner_does_not_invent_numbers() {
        assert_eq!(clean_decimal("1,2,3"), None);
        assert_eq!(clean_decimal("1234,56"), None);
        assert_eq!(clean_decimal("1 000"), None);
        assert_eq!(clean_decimal("1e3"), None);
        assert_eq!(clean_decimal("+-5"), None);
        assert_eq!(clean_decimal("(-5)"), None);
        assert_eq!(clean_decimal("5-"), None);
        assert_eq!(clean_decimal("1.2.3"), None);
        assert_eq!(clean_decimal("."), None);
        // …while every shape the statements do use still parses.
        assert_eq!(clean_decimal("1,234,567.89"), Some(dec!(1234567.89)));
        assert_eq!(clean_decimal("$ 1,234.56"), Some(dec!(1234.56)));
    }

    #[test]
    fn excel_refs_lose_their_wrapper() {
        assert_eq!(clean_str("=\"9000000000001\""), "9000000000001");
        assert_eq!(clean_str("\"quoted\""), "quoted");
        assert_eq!(clean_str(" plain "), "plain");
    }

    #[test]
    fn quantities_keep_their_sign() {
        assert_eq!(clean_qty("+2"), Some(2));
        assert_eq!(clean_qty("-4"), Some(-4));
        assert_eq!(clean_qty(""), None);
    }

    fn record(fields: &[&str]) -> StringRecord {
        StringRecord::from(fields.to_vec())
    }

    #[test]
    fn section_titles_are_recognised_and_impostors_are_not() {
        assert_eq!(
            section_header(&record(&["Cash Balance"])),
            Some(Kind::CashBalance)
        );
        assert_eq!(section_header(&record(&["Options"])), Some(Kind::Options));
        assert_eq!(
            section_header(&record(&["Account Summary"])),
            Some(Kind::AccountSummary)
        );
        assert_eq!(
            section_header(&record(&["Equities"])),
            Some(Kind::Ignored("Equities".into()))
        );
        assert_eq!(
            section_header(&record(&[
                "Crypto # (Crypto offered by Charles Schwab Premier Bank, SSB) Statements"
            ])),
            Some(Kind::Ignored("Crypto Statements".into()))
        );
        // The stray total and the lone-space line are not sections.
        assert_eq!(section_header(&record(&["Total Cash $79,984.05"])), None);
        assert_eq!(section_header(&record(&[" "])), None);
        assert_eq!(
            section_header(&record(&["", "OVERALL TOTALS", "", "$1.00"])),
            None
        );
        // An unknown but title-shaped row is treated as a section we skip.
        assert_eq!(
            section_header(&record(&["Bond Statements"])),
            Some(Kind::Ignored("Bond Statements".into()))
        );
    }

    /// A synthetic statement for the cases the fixtures cannot show: a file
    /// name that disagrees with the rows, an unknown section, an
    /// option-shaped cash row that does not parse, and a contract whose root
    /// we have no settlement rule for.
    const SYNTHETIC: &str = "\
Account Statement for 00000000TESTSCHW (Individual) since 1/1/26 through 1/1/26

Cash Balance
DATE,TIME,TYPE,REF #,DESCRIPTION,Misc Fees,Commissions & Fees,AMOUNT,BALANCE
8/28/26,00:00:00,BAL,,Cash balance at the start of business day 28.08 CST,,,,\"80,000.00\"
8/28/26,09:45:29,TRD,=\"9000000000054\",BOT +1 VERTICAL SPX 100 19 FEB 27 [AM] 6800/6700 PUT @8.50 CBOE,-1.10,-1.30,-850.00,\"79,150.00\"
8/28/26,09:46:00,TRD,=\"9000000000998\",BOT +1 DUMMY @127.11,,,-127.11,\"79,022.89\"
8/28/26,09:47:00,TRD,=\"9000000000999\",BOT +1 SPX 100 19 FEB 27 [AM] 6800 CHICKEN @8.50 CBOE,,,-850.00,\"78,172.89\"
,,,,TOTAL,($1.10),($1.30),($850.00),\"$79,150.00\"

Bond Statements
Trade Date,Type,Amount
 
\"Total Cash $79,150.00\"

Account Trade History
Exec Time,Spread,Side,Qty,Symbol,Exp,Strike,Type,Price,Net Price,Opra
8/28/26 09:45:29,VERTICAL,BUY,+1,SPX,19 FEB 27,6800,PUT,89.62,8.50,SPX270219P6800
,,SELL,-1,SPX,19 FEB 27,6700,PUT,81.12,DEBIT,SPX270219P6700

Options
Symbol,Option Code,Exp,Strike,Type,Qty,Trade Price,Mark,Mark Value
SPX,SPX270219P6800,19 FEB 27,6800,PUT,+1,89.62,.725,$72.50
XYZ,XYZ270219P50,19 FEB 27,50,PUT,+1,1.00,1.00,$100.00
,OVERALL TOTALS,,,,,,,\"$72.50\"
Account Summary
Net Liquidating Value,\"$79,150.00\"
Crypto Trading Fees YTD,N/A
";

    /// As TOS writes it: a BOM and CRLF line endings.
    fn synthetic() -> String {
        format!("\u{feff}{}", SYNTHETIC.replace('\n', "\r\n"))
    }

    fn parse_synthetic(file_name: &str) -> Statement {
        parse_str(
            &synthetic(),
            Path::new("/tmp/statements").join(file_name).as_path(),
            DEFAULT_TIMEZONE,
        )
        .expect("parses")
    }

    #[test]
    fn bom_crlf_and_the_stray_total_cash_row_are_survivable() {
        let s = parse_synthetic("2026-08-28-AccountStatement.csv");
        assert_eq!(s.as_of.to_string(), "2026-08-28");
        assert_eq!(s.executed_orders.len(), 1);
        assert_eq!(s.executed_orders[0].fills.len(), 2);
        assert_eq!(s.executed_orders[0].net_price, Some(dec!(8.50)));
        assert_eq!(
            s.executed_orders[0].ref_no.as_deref(),
            Some("9000000000054")
        );
        assert_eq!(s.executed_orders[0].fees.total(), dec!(2.40));
        assert_eq!(s.account_summary.net_liquidating_value(), Some(dec!(79150)));
        assert_eq!(s.account_summary.get("Crypto Trading Fees YTD"), None);
        assert_eq!(
            s.header_range.map(|(a, _)| a.to_string()),
            Some("2026-01-01".to_string()),
            "the header line is parsed but never trusted"
        );
    }

    #[test]
    fn a_file_name_that_disagrees_with_the_rows_warns_and_the_rows_win() {
        let s = parse_synthetic("2026-08-27-AccountStatement.csv");
        assert_eq!(s.as_of.to_string(), "2026-08-28");
        let warning = s
            .warnings
            .iter()
            .find(|w| w.code() == "statement_date_disagreement")
            .expect("a disagreement warning");
        assert!(warning.message().contains("2026-08-27"));
        assert!(warning.message().contains("2026-08-28"));
    }

    #[test]
    fn a_statement_with_no_date_anywhere_is_an_error() {
        let s = parse_str(
            &synthetic(),
            Path::new("AccountStatement.csv"),
            DEFAULT_TIMEZONE,
        )
        .expect("dated rows are enough");
        assert_eq!(s.as_of.to_string(), "2026-08-28");
        let err = parse_str("Cash Balance\r\n", Path::new("nope.csv"), DEFAULT_TIMEZONE)
            .expect_err("nothing to date it by");
        assert!(err.message.contains("no dated rows"));
    }

    #[test]
    fn unknown_sections_and_unparseable_rows_are_reported_not_swallowed() {
        let s = parse_synthetic("2026-08-28-AccountStatement.csv");
        assert!(
            s.warnings.iter().any(|w| matches!(
                w,
                Warning::StatementSectionSkipped { section, .. } if section == "Bond Statements"
            )),
            "an unrecognised section must be reported: {:?}",
            s.warnings
        );
        // The equity row is dropped silently; the option-shaped one is not.
        assert_eq!(s.cash_trades.len(), 1);
        let skipped: Vec<String> = s
            .warnings
            .iter()
            .filter(|w| w.code() == "statement_row_skipped")
            .map(Warning::message)
            .collect();
        assert_eq!(skipped.len(), 2, "{skipped:?}");
        assert!(skipped
            .iter()
            .any(|m| m.contains("Cash Balance") && m.contains("CHICKEN")));
        // XYZ has no settlement rule, so the position is skipped, not guessed.
        assert_eq!(s.option_positions.len(), 1);
        assert!(skipped.iter().any(|m| m.contains("XYZ270219P50")));
    }

    #[test]
    fn header_range_is_read_but_not_trusted() {
        assert_eq!(
            parse_header_range(
                "Account Statement for 00000000TESTSCHW (Individual) since 8/27/26 through 8/27/26"
            ),
            Some((
                NaiveDate::from_ymd_opt(2026, 8, 27).unwrap(),
                NaiveDate::from_ymd_opt(2026, 8, 27).unwrap()
            ))
        );
        assert_eq!(parse_header_range("Cash Balance"), None);
    }
}
