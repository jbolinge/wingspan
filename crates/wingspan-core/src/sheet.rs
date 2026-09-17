//! The Legs table parser: a Google-Sheet tab or CSV file, read into
//! source-agnostic [`crate::legs::LegRow`]s.
//!
//! This module is pure: no I/O, no clock, no filesystem. It reads whatever
//! bytes were already fetched (by the app layer) or loaded from disk (by a
//! CSV file import) and turns them into rows plus warnings. The parsing
//! rules are specified in `docs/DATA-FORMATS.md`'s "The Legs table" section;
//! that document is the authority for every rule below, and it is not
//! edited by this change.

use std::collections::BTreeMap;

use chrono::{NaiveDate, NaiveDateTime};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::Serialize;

use crate::contract::OptionContract;
use crate::legs::{LegOrigin, LegRow};
use crate::statement::clean_decimal;
use crate::warning::Warning;

/// Which [`LegOrigin`] the parser stamps on each row it produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowOrigin {
    /// Rows fetched from a Google Sheet.
    Sheet,
    /// Rows read from a local CSV file.
    Csv,
}

/// One logical column of the Legs table. `Trade`, `Side`, `Qty` and `Occ`
/// are required; `Opened` and `OpenPrice` are expected; the rest are
/// optional cross-checks or human labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Column {
    Trade,
    Side,
    Qty,
    Occ,
    Opened,
    OpenPrice,
    Closed,
    ClosePrice,
    Status,
    Fees,
    Mark,
    Expiry,
    InstanceNo,
    Key,
    Structure,
    Leg,
    Pnl,
}

impl Column {
    /// Canonical order, matching `docs/IPC.md`'s `columns_found` example
    /// for its first fourteen entries.
    pub const ALL: [Column; 17] = [
        Column::Trade,
        Column::Side,
        Column::Qty,
        Column::Occ,
        Column::Opened,
        Column::OpenPrice,
        Column::Closed,
        Column::ClosePrice,
        Column::Status,
        Column::Fees,
        Column::Mark,
        Column::Expiry,
        Column::InstanceNo,
        Column::Key,
        Column::Structure,
        Column::Leg,
        Column::Pnl,
    ];

    /// The header name this column is displayed/written as (e.g. in
    /// `columns_found`/`columns_missing`/`columns_absent`, and by
    /// `wingspan-cli export-legs`).
    #[must_use]
    pub fn display(self) -> &'static str {
        match self {
            Column::Trade => "Trade",
            Column::Side => "Side",
            Column::Qty => "Qty",
            Column::Occ => "OCC Symbol",
            Column::Opened => "Opened",
            Column::OpenPrice => "Open Price",
            Column::Closed => "Closed",
            Column::ClosePrice => "Close Price",
            Column::Status => "Status",
            Column::Fees => "Fees",
            Column::Mark => "Mark",
            Column::Expiry => "Expiry",
            Column::InstanceNo => "#",
            Column::Key => "Key",
            Column::Structure => "Structure",
            Column::Leg => "Leg",
            Column::Pnl => "P/L",
        }
    }

    /// Header names this column is recognised under, already run through
    /// [`normalise`].
    #[must_use]
    pub fn aliases(self) -> &'static [&'static str] {
        match self {
            Column::Trade => &["trade", "trade name", "name", "trade id"],
            Column::Side => &["side", "b s", "buy sell", "action"],
            Column::Qty => &["qty", "quantity", "contracts", "size"],
            Column::Occ => &[
                "occ symbol",
                "occ",
                "symbol",
                "tos code",
                "tos symbol",
                "option symbol",
            ],
            Column::Opened => &["opened", "open date", "entry date", "date opened"],
            Column::OpenPrice => &["open price", "entry price", "avg open"],
            Column::Closed => &["closed", "close date", "exit date"],
            Column::ClosePrice => &["close price", "exit price"],
            Column::Status => &["status", "state"],
            Column::Fees => &["fees", "commissions", "commissions fees", "fees comm"],
            Column::Mark => &["mark", "last", "current price"],
            Column::Expiry => &["expiry", "expiration", "exp", "exp date"],
            Column::InstanceNo => &["#", "no", "trade no", "trade #"],
            Column::Key => &["key", "lot key", "row key"],
            Column::Structure => &["structure", "strategy"],
            Column::Leg => &["leg", "description", "leg description"],
            Column::Pnl => &["p l", "pnl"],
        }
    }

    /// `Trade`, `Side`, `Qty`, `OCC Symbol` — a header missing one of these
    /// refuses the whole import.
    #[must_use]
    pub fn is_required(self) -> bool {
        matches!(
            self,
            Column::Trade | Column::Side | Column::Qty | Column::Occ
        )
    }

    /// `Opened`, `Open Price` — absent (or entirely blank) is accepted with
    /// one `sheet_column_absent` each.
    #[must_use]
    pub fn is_expected(self) -> bool {
        matches!(self, Column::Opened | Column::OpenPrice)
    }
}

/// What a Legs table holds, as `probe_trade_source` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct SheetSummary {
    pub columns_found: Vec<String>,
    pub columns_missing: Vec<String>,
    pub columns_absent: Vec<String>,
    pub columns_ignored: Vec<String>,
    pub rows: usize,
    pub legs: usize,
    pub rows_skipped: usize,
    /// 1-based record index of the header; 0 when no header was found.
    pub header_row: usize,
}

/// Lowercase; keep ASCII alphanumerics and `#`; collapse every run of other
/// characters into a single space; trim.
///
/// `docs/DATA-FORMATS.md` describes this as "every run of non-alphanumerics
/// collapsed to one space", but under that literal rule the documented
/// alias `#` normalises to the empty string and `trade #` collides with
/// `trade`. Keeping `#` alongside alphanumerics is the only rule that makes
/// the documented aliases (`#`, `trade #`) actually resolve — the doc's
/// phrasing is imprecise here, not a mismatch to fix by changing this code.
fn normalise(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut pending_space = false;
    for ch in raw.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() || lower == '#' {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push(lower);
        } else {
            pending_space = true;
        }
    }
    out
}

/// Strip a single leading UTF-8 BOM, if present.
fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{FEFF}').unwrap_or(s)
}

/// What one header record resolved to.
#[derive(Debug, Clone, Default)]
struct HeaderResolution {
    /// First occurrence of each logical column, by field index.
    found: BTreeMap<Column, usize>,
    /// Extra occurrences of an already-found column, in header order.
    duplicates: Vec<Column>,
    /// Raw trimmed text of header cells that resolved to nothing, in
    /// header order.
    ignored: Vec<String>,
}

/// Resolve one candidate header record. `is_first_record` gates the BOM
/// strip, which applies only to the very first field of the very first
/// record of the whole file (not necessarily this header, when a title
/// block sits above it).
fn resolve_header(record: &csv::StringRecord, is_first_record: bool) -> HeaderResolution {
    let mut resolution = HeaderResolution::default();
    for (i, raw_cell) in record.iter().enumerate() {
        let base = if is_first_record && i == 0 {
            strip_bom(raw_cell)
        } else {
            raw_cell
        };
        let norm = normalise(base);
        if norm.is_empty() {
            // Trailing empty header cells are normal; neither found nor
            // ignored.
            continue;
        }
        match Column::ALL
            .into_iter()
            .find(|c| c.aliases().iter().any(|a| *a == norm))
        {
            Some(col) => match resolution.found.entry(col) {
                std::collections::btree_map::Entry::Vacant(e) => {
                    e.insert(i);
                }
                std::collections::btree_map::Entry::Occupied(_) => {
                    resolution.duplicates.push(col);
                }
            },
            None => resolution.ignored.push(raw_cell.trim().to_string()),
        }
    }
    resolution
}

/// The trimmed cell text at `idx` in `rec`, or `""` when the column was not
/// found in the header or the row is short of it (the `csv` crate's
/// `flexible(true)` mode allows ragged rows).
fn cell<'a>(rec: &'a csv::StringRecord, idx: Option<&usize>) -> &'a str {
    idx.and_then(|&i| rec.get(i)).unwrap_or("").trim()
}

/// `true` when `col` is either absent from the header or present but blank
/// in every considered row.
fn column_all_blank(
    col: Column,
    considered: &[(u64, &csv::StringRecord)],
    found: &BTreeMap<Column, usize>,
) -> bool {
    match found.get(&col) {
        None => true,
        Some(&idx) => considered
            .iter()
            .all(|(_, rec)| rec.get(idx).unwrap_or("").trim().is_empty()),
    }
}

/// Try each of the Legs table's accepted date formats in order. Sheets
/// exports a column in its *display* format, so `7/14/2026` with no
/// leading zeros is the common case.
fn parse_date(cell: &str) -> Option<NaiveDate> {
    if cell.is_empty() {
        return None;
    }
    if let Ok(d) = NaiveDate::parse_from_str(cell, "%Y-%m-%d") {
        return Some(d);
    }
    // `%m/%d/%Y` and `%m/%d/%y` both cover `m/d/y`-shaped cells, but
    // chrono's `%Y` accepts any digit width when parsing — it would happily
    // read `7/14/26` as the literal year 26 before `%y` is ever tried. Every
    // date this table carries is in the 2000s, so the century is decided by
    // hand from how many digits the trailing token actually has, rather
    // than trusting chrono's format-string fallthrough to pick the right
    // one.
    let mut parts = cell.splitn(3, '/');
    if let (Some(m), Some(d), Some(y)) = (parts.next(), parts.next(), parts.next()) {
        if y.len() >= 4 {
            if let Ok(date) = NaiveDate::parse_from_str(cell, "%m/%d/%Y") {
                return Some(date);
            }
        } else if !y.is_empty() && y.chars().all(|c| c.is_ascii_digit()) {
            if let (Ok(month), Ok(day), Ok(year)) =
                (m.parse::<u32>(), d.parse::<u32>(), y.parse::<i32>())
            {
                if let Some(date) = NaiveDate::from_ymd_opt(year + 2000, month, day) {
                    return Some(date);
                }
            }
        }
    }
    NaiveDate::parse_from_str(cell, "%b %d %Y").ok()
}

/// Parse a Legs table (a Google-Sheet tab exported to CSV, or a CSV file
/// with the same columns) into source-agnostic leg rows. See
/// `docs/DATA-FORMATS.md`'s "The Legs table" for the full rule set; this
/// function implements it directly, in the same order the doc lists it.
#[must_use]
pub fn parse_csv(text: &str, origin: RowOrigin) -> (Vec<LegRow>, SheetSummary, Vec<Warning>) {
    let mut warnings: Vec<Warning> = Vec::new();
    let mut summary = SheetSummary::default();

    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let records: Vec<csv::StringRecord> = reader.records().filter_map(Result::ok).collect();

    if records.is_empty() {
        for col in Column::ALL.into_iter().filter(|c| c.is_required()) {
            warnings.push(Warning::SheetColumnMissing {
                column: col.display().into(),
            });
            summary.columns_missing.push(col.display().into());
        }
        return (Vec::new(), summary, warnings);
    }

    // Scan the first five records for the one that resolves every required
    // column; remember record 0's resolution along the way as the fallback
    // header when none of the first five does.
    let mut resolution_at_0: Option<HeaderResolution> = None;
    let mut chosen: Option<(usize, HeaderResolution)> = None;
    for (idx, record) in records.iter().enumerate().take(5) {
        let res = resolve_header(record, idx == 0);
        if idx == 0 {
            resolution_at_0 = Some(res.clone());
        }
        if Column::ALL
            .into_iter()
            .filter(|c| c.is_required())
            .all(|c| res.found.contains_key(&c))
        {
            chosen = Some((idx, res));
            break;
        }
    }

    let (header_idx, resolution, header_ok) = match chosen {
        Some((idx, res)) => (idx, res, true),
        None => (
            0,
            resolution_at_0.expect("records is non-empty in this branch"),
            false,
        ),
    };

    summary.header_row = header_idx + 1;

    if header_idx > 0 {
        warnings.push(Warning::SheetHeaderOffset {
            row: header_idx as u64 + 1,
        });
    }
    for col in &resolution.duplicates {
        warnings.push(Warning::SheetDuplicateColumn {
            column: col.display().into(),
        });
    }

    for col in Column::ALL {
        if resolution.found.contains_key(&col) {
            summary.columns_found.push(col.display().into());
        }
    }
    summary.columns_ignored.clone_from(&resolution.ignored);

    if !header_ok {
        for col in Column::ALL.into_iter().filter(|c| c.is_required()) {
            if !resolution.found.contains_key(&col) {
                warnings.push(Warning::SheetColumnMissing {
                    column: col.display().into(),
                });
                summary.columns_missing.push(col.display().into());
            }
        }
        return (Vec::new(), summary, warnings);
    }

    let found = resolution.found;

    // Two passes are needed: an entirely-blank expected column must become
    // one `sheet_column_absent`, not one per row, so the "is this column
    // blank everywhere" question is answered before the row loop uses it.
    let considered: Vec<(u64, &csv::StringRecord)> = records
        .iter()
        .enumerate()
        .skip(header_idx + 1)
        .filter(|(_, rec)| rec.iter().any(|f| !f.trim().is_empty()))
        .map(|(i, rec)| (i as u64 + 1, rec))
        .collect();
    summary.rows = considered.len();

    let opened_all_blank = column_all_blank(Column::Opened, &considered, &found);
    let open_price_all_blank = column_all_blank(Column::OpenPrice, &considered, &found);
    let open_price_in_header = found.contains_key(&Column::OpenPrice);

    let mut leg_rows: Vec<LegRow> = Vec::with_capacity(considered.len());

    for (row, rec) in considered.iter().copied() {
        let trade_cell = cell(rec, found.get(&Column::Trade));
        if trade_cell.is_empty() {
            warnings.push(Warning::SheetRowSkipped {
                row,
                reason: format!("{} is blank", Column::Trade.display()),
            });
            continue;
        }
        let side_cell = cell(rec, found.get(&Column::Side));
        if side_cell.is_empty() {
            warnings.push(Warning::SheetRowSkipped {
                row,
                reason: format!("{} is blank", Column::Side.display()),
            });
            continue;
        }
        let qty_cell = cell(rec, found.get(&Column::Qty));
        if qty_cell.is_empty() {
            warnings.push(Warning::SheetRowSkipped {
                row,
                reason: format!("{} is blank", Column::Qty.display()),
            });
            continue;
        }
        let occ_cell = cell(rec, found.get(&Column::Occ));
        if occ_cell.is_empty() {
            warnings.push(Warning::SheetRowSkipped {
                row,
                reason: format!("{} is blank", Column::Occ.display()),
            });
            continue;
        }

        let side_sign: i32 = match side_cell.to_ascii_lowercase().as_str() {
            "buy" | "b" | "bot" | "bought" | "bto" | "btc" => 1,
            "sell" | "s" | "sld" | "sold" | "sto" | "stc" => -1,
            _ => {
                warnings.push(Warning::SheetRowSkipped {
                    row,
                    reason: format!("unreadable side {side_cell:?}"),
                });
                continue;
            }
        };

        let Some(qty_raw) = clean_decimal(qty_cell) else {
            warnings.push(Warning::SheetRowSkipped {
                row,
                reason: "unreadable quantity".to_string(),
            });
            continue;
        };
        let qty_trunc = qty_raw.trunc();
        let Some(magnitude) = qty_trunc.abs().to_i32().filter(|m| *m != 0) else {
            warnings.push(Warning::SheetRowSkipped {
                row,
                reason: "quantity is zero".to_string(),
            });
            continue;
        };
        let qty = side_sign * magnitude;
        let cell_sign = if qty_raw.is_sign_negative() { -1 } else { 1 };
        if cell_sign != side_sign {
            warnings.push(Warning::SheetValueDisagrees {
                row,
                column: "Qty".into(),
                sheet: qty_cell.to_string(),
                derived: qty.to_string(),
            });
        }

        let contract = match OptionContract::from_tos_code(occ_cell, "SPX", 100) {
            Ok(c) => c,
            Err(tos_err) => {
                let osi_attempt = if occ_cell.chars().count() == 21 {
                    OptionContract::from_osi(occ_cell, "SPX", 100).ok()
                } else {
                    None
                };
                if let Some(c) = osi_attempt {
                    c
                } else {
                    warnings.push(Warning::SheetRowSkipped {
                        row,
                        reason: format!("not an SPX option: {tos_err}"),
                    });
                    continue;
                }
            }
        };
        let contract_str = contract.to_tos_code();

        let closed_date = parse_date(cell(rec, found.get(&Column::Closed)));
        let open_price = clean_decimal(cell(rec, found.get(&Column::OpenPrice)));
        let close_price = clean_decimal(cell(rec, found.get(&Column::ClosePrice)));
        let status_cell = cell(rec, found.get(&Column::Status));
        let status = (!status_cell.is_empty()).then(|| status_cell.to_string());
        let fees = clean_decimal(cell(rec, found.get(&Column::Fees)))
            .unwrap_or(Decimal::ZERO)
            .abs();
        let mark = clean_decimal(cell(rec, found.get(&Column::Mark)));
        let structure_cell = cell(rec, found.get(&Column::Structure));
        let structure = (!structure_cell.is_empty()).then(|| structure_cell.to_string());
        let leg_cell = cell(rec, found.get(&Column::Leg));
        let label = (!leg_cell.is_empty()).then(|| leg_cell.to_string());
        let sheet_pnl = clean_decimal(cell(rec, found.get(&Column::Pnl)));
        let expiry_cell = cell(rec, found.get(&Column::Expiry));

        let instance_no = clean_decimal(cell(rec, found.get(&Column::InstanceNo))).and_then(|d| {
            let t = d.trunc();
            if t > Decimal::ZERO {
                t.to_u32()
            } else {
                None
            }
        });

        let key_cell = cell(rec, found.get(&Column::Key));
        let mut instance: Option<NaiveDate> = None;
        let mut instant: Option<NaiveDateTime> = None;
        if !key_cell.is_empty() {
            if let Some(field0) = key_cell.split('|').next() {
                if let Some((_, date_part)) = field0.trim().rsplit_once(char::is_whitespace) {
                    instance = NaiveDate::parse_from_str(date_part.trim(), "%Y-%m-%d").ok();
                }
            }
            if let Some(field2) = key_cell.split('|').nth(2) {
                instant = NaiveDateTime::parse_from_str(field2.trim(), "%Y-%m-%d %H:%M:%S").ok();
            }
        }

        let opened_on = parse_date(cell(rec, found.get(&Column::Opened)))
            .or(instance)
            .or(closed_date);

        if open_price_in_header && !open_price_all_blank && open_price.is_none() {
            warnings.push(Warning::SheetLegUnpriced {
                row,
                trade: trade_cell.to_string(),
                contract: contract_str.clone(),
            });
        }

        if closed_date.is_some() && close_price.is_none() {
            warnings.push(Warning::SheetClosePriceMissing {
                row,
                trade: trade_cell.to_string(),
                contract: contract_str.clone(),
            });
        }

        if let Some(expiry_date) = parse_date(expiry_cell) {
            if expiry_date != contract.expiry {
                warnings.push(Warning::SheetValueDisagrees {
                    row,
                    column: "Expiry".into(),
                    sheet: expiry_cell.to_string(),
                    derived: contract.expiry.to_string(),
                });
            }
        }

        let derived_status = if closed_date.is_some() && close_price.is_some() {
            "Closed"
        } else {
            "Open"
        };
        if !status_cell.is_empty() && !status_cell.eq_ignore_ascii_case(derived_status) {
            warnings.push(Warning::SheetValueDisagrees {
                row,
                column: "Status".into(),
                sheet: status_cell.to_string(),
                derived: derived_status.to_string(),
            });
        }

        if let (Some(sheet_pnl_val), Some(open)) = (sheet_pnl, open_price) {
            let sign = Decimal::from(qty.signum());
            let mult = Decimal::from(contract.multiplier);
            let n = Decimal::from(magnitude);
            let computed = if let Some(close) = close_price {
                Some((close - open) * n * mult * sign - fees)
            } else {
                mark.map(|m| (m - open) * n * mult * sign - fees)
            };
            if let Some(computed) = computed {
                if (sheet_pnl_val - computed).abs() > Decimal::new(1, 2) {
                    warnings.push(Warning::SheetPnlDisagreement {
                        row,
                        sheet: sheet_pnl_val,
                        computed,
                    });
                }
            }
        }

        let origin_tag = match origin {
            RowOrigin::Sheet => LegOrigin::Sheet { row },
            RowOrigin::Csv => LegOrigin::Csv { row },
        };

        leg_rows.push(LegRow {
            trade: trade_cell.to_string(),
            instance_no,
            instance,
            contract,
            qty,
            opened_on,
            open_price,
            closed_on: closed_date,
            close_price,
            status,
            fees,
            mark,
            structure,
            label,
            origin: origin_tag,
            sheet_pnl,
            instant,
        });
    }

    if opened_all_blank {
        warnings.push(Warning::SheetColumnAbsent {
            column: Column::Opened.display().into(),
        });
        summary.columns_absent.push(Column::Opened.display().into());
    }
    if open_price_all_blank {
        warnings.push(Warning::SheetColumnAbsent {
            column: Column::OpenPrice.display().into(),
        });
        summary
            .columns_absent
            .push(Column::OpenPrice.display().into());
    }

    summary.legs = leg_rows.len();
    summary.rows_skipped = summary.rows - summary.legs;

    (leg_rows, summary, warnings)
}
