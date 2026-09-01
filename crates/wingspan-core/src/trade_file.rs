//! Trade files: one file per trade, one TOS order string per line.
//!
//! The file *name* carries the trade name and its open date and is parsed
//! **right to left**: `<name…> <Month(full|abbrev)> <d{1,2}> <yyyy> [Closed]`.
//! Everything before the date is the name, spaces and all (`Delta 1`).
//!
//! Inside the file, blank lines and `#` comment lines are ignored, orders may
//! appear out of chronological order, and a line that does not parse costs
//! that line only — never the file.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;

use crate::dates::month_number;
use crate::error::ParseError;
use crate::order::OrderSource;
use crate::tos_order;
use crate::trade::{Trade, TradeStatus};
use crate::warning::Warning;

/// What a trade-file name says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeFileName {
    pub name: String,
    pub opened_on: NaiveDate,
    /// The name ended in `Closed`.
    pub closed: bool,
}

/// Parse `M1 July 14 2026`, `M4 Aug 4 2026`, `Delta 1 Aug 25 2026 Closed`.
///
/// Returns `None` for anything that does not end in a date (which is how
/// stray files in the trades folders get skipped).
#[must_use]
pub fn parse_file_name(file_name: &str) -> Option<TradeFileName> {
    let mut tokens: Vec<&str> = file_name.split_whitespace().collect();
    // Tolerate an extension on the last token (`… 2026.txt`).
    if let Some(last) = tokens.last_mut() {
        if let Some((stem, ext)) = last.rsplit_once('.') {
            if !stem.is_empty() && !ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric())
            {
                *last = stem;
            }
        }
    }
    let closed = tokens
        .last()
        .is_some_and(|t| t.eq_ignore_ascii_case("closed"));
    if closed {
        tokens.pop();
    }
    if tokens.len() < 4 {
        return None;
    }
    let n = tokens.len();
    let year_token = tokens[n - 1];
    if year_token.len() != 4 || !year_token.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year: i32 = year_token.parse().ok()?;
    let day: u32 = tokens[n - 2].parse().ok()?;
    let month = month_number(tokens[n - 3])?;
    let opened_on = NaiveDate::from_ymd_opt(year, month, day)?;
    let name = tokens[..n - 3].join(" ");
    if name.is_empty() {
        return None;
    }
    Some(TradeFileName {
        name,
        opened_on,
        closed,
    })
}

/// One parsed trade file plus everything questionable about it.
#[derive(Debug, Clone)]
pub struct ParsedTradeFile {
    pub trade: Trade,
    pub warnings: Vec<Warning>,
    /// Lines that did not parse (also reported as `OrderParseFailed`
    /// warnings); kept so callers can show the offending token.
    pub errors: Vec<ParseError>,
}

/// Read and parse one trade file.
///
/// `dir_status` is the status implied by the directory the file lives in; a
/// `Closed` suffix in the name wins over `Open`.
pub fn parse(path: &Path, dir_status: TradeStatus) -> Result<ParsedTradeFile, ParseError> {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| ParseError::new("path has no file name"))?;
    let parsed_name = parse_file_name(&file_name).ok_or_else(|| {
        ParseError::new(
            "file name does not end in `<Month> <day> <year> [Closed]`; not a trade file",
        )
        .with_token(file_name.clone())
    })?;
    let bytes = fs::read(path)
        .map_err(|e| ParseError::new(format!("cannot read trade file: {e}")).at(path, 0))?;
    let text = String::from_utf8_lossy(&bytes);
    // `from_utf8_lossy` only allocates when it had to replace something, which
    // is the one signal that the file is not the text we think it is.
    let lossy = matches!(text, std::borrow::Cow::Owned(_));
    let mut parsed = parse_str(&text, path, &parsed_name, dir_status)?;
    if lossy {
        parsed.warnings.insert(
            0,
            Warning::NonUtf8File {
                path: path.to_path_buf(),
            },
        );
    }
    Ok(parsed)
}

/// Parse the contents of a trade file that has already been read.
pub fn parse_str(
    text: &str,
    path: &Path,
    parsed_name: &TradeFileName,
    dir_status: TradeStatus,
) -> Result<ParsedTradeFile, ParseError> {
    let status = if parsed_name.closed || dir_status == TradeStatus::Closed {
        TradeStatus::Closed
    } else {
        TradeStatus::Open
    };
    let mut orders = Vec::new();
    let mut warnings = Vec::new();
    let mut errors = Vec::new();

    for (index, line) in text.lines().enumerate() {
        let line_no = index + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        match tos_order::parse_with_warnings(line) {
            Ok((mut order, mut order_warnings)) => {
                order.source = Some(OrderSource {
                    path: path.to_path_buf(),
                    line: line_no,
                });
                warnings.append(&mut order_warnings);
                orders.push(order);
            }
            Err(error) => {
                let error = error.at(path, line_no);
                warnings.push(Warning::OrderParseFailed {
                    path: path.to_path_buf(),
                    line: line_no,
                    message: format!(
                        "{}{}",
                        error.message,
                        error
                            .token
                            .as_ref()
                            .map(|t| format!(" (at {t:?})"))
                            .unwrap_or_default()
                    ),
                });
                errors.push(error);
            }
        }
    }

    Ok(ParsedTradeFile {
        trade: Trade {
            id: Trade::slug(&parsed_name.name, parsed_name.opened_on),
            name: parsed_name.name.clone(),
            opened_on: parsed_name.opened_on,
            closed_on: None,
            status,
            source_path: path.to_path_buf(),
            orders,
        },
        warnings,
        errors,
    })
}

/// Parse every trade file in a directory, in file-name order.
///
/// Dotfiles (`.DS_Store`), sub-directories and files whose names do not end in
/// a date are skipped with a warning; nothing here ever fails the whole load.
#[must_use]
pub fn load_dir(dir: &Path, dir_status: TradeStatus) -> (Vec<Trade>, Vec<Warning>) {
    let mut trades = Vec::new();
    let mut warnings = Vec::new();

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            warnings.push(Warning::TradeFileSkipped {
                path: dir.to_path_buf(),
                reason: format!("cannot read directory: {e}"),
            });
            return (trades, warnings);
        }
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();

    for path in paths {
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if file_name.starts_with('.') {
            warnings.push(Warning::TradeFileSkipped {
                path: path.clone(),
                reason: "dotfile".into(),
            });
            continue;
        }
        // Filtering these out before the loop is what used to make a user who
        // organises trades into sub-folders get an empty book and no
        // explanation; a broken symlink went the same way.
        if !path.is_file() {
            warnings.push(Warning::TradeFileSkipped {
                path: path.clone(),
                reason: if path.is_dir() {
                    "a directory, not a trade file".into()
                } else {
                    "not a readable file".into()
                },
            });
            continue;
        }
        match parse(&path, dir_status) {
            Ok(parsed) => {
                warnings.extend(parsed.warnings);
                trades.push(parsed.trade);
            }
            Err(error) => warnings.push(Warning::TradeFileSkipped {
                path: path.clone(),
                reason: error.message,
            }),
        }
    }
    (trades, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn names_parse_right_to_left() {
        let n = parse_file_name("M1 July 14 2026").unwrap();
        assert_eq!(n.name, "M1");
        assert_eq!(n.opened_on, d(2026, 7, 14));
        assert!(!n.closed);

        let n = parse_file_name("Delta 1 Aug 25 2026 Closed").unwrap();
        assert_eq!(n.name, "Delta 1");
        assert_eq!(n.opened_on, d(2026, 8, 25));
        assert!(n.closed);

        // The name may itself contain month-like words and digits.
        let n = parse_file_name("May Fly 2 Sep 1 2026").unwrap();
        assert_eq!(n.name, "May Fly 2");
        assert_eq!(n.opened_on, d(2026, 9, 1));

        // A non-trading open date is taken as written (Aug 15 2026 is a Saturday).
        assert_eq!(
            parse_file_name("M5 Aug 15 2026").unwrap().opened_on,
            d(2026, 8, 15)
        );

        // Extensions are tolerated.
        assert_eq!(
            parse_file_name("M1 July 14 2026.txt").unwrap().opened_on,
            d(2026, 7, 14)
        );
    }

    #[test]
    fn junk_names_are_rejected() {
        for bad in [
            ".DS_Store",
            "notes",
            "July 14 2026",  // no name
            "M1 July 14 26", // two-digit year
            "M1 Smarch 14 2026",
            "M1 July 32 2026",
            "",
        ] {
            assert!(parse_file_name(bad).is_none(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn blank_and_comment_lines_are_ignored_and_bad_lines_cost_one_line() {
        let text = "\n# a note\nBUY +1 SPX 100 18 SEP 26 [AM] 5050 PUT @4.90 LMT\nnonsense\n";
        let name = parse_file_name("BSH July 14 2026").unwrap();
        let parsed = parse_str(
            text,
            Path::new("/x/BSH July 14 2026"),
            &name,
            TradeStatus::Open,
        )
        .unwrap();
        assert_eq!(parsed.trade.orders.len(), 1);
        assert_eq!(parsed.trade.orders[0].source.as_ref().unwrap().line, 3);
        assert_eq!(parsed.errors.len(), 1);
        assert_eq!(parsed.errors[0].line, Some(4));
        assert_eq!(parsed.warnings.len(), 1);
        assert_eq!(parsed.warnings[0].code(), "order_parse_failed");
        assert_eq!(parsed.trade.id, "bsh-2026-07-14");
        assert_eq!(parsed.trade.status, TradeStatus::Open);
    }

    #[test]
    fn closed_suffix_wins_over_the_directory() {
        let name = parse_file_name("LPTA July 24 2026 Closed").unwrap();
        let parsed = parse_str("", Path::new("/x/y"), &name, TradeStatus::Open).unwrap();
        assert_eq!(parsed.trade.status, TradeStatus::Closed);
    }
}
