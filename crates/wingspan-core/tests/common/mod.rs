//! Shared helpers for the fixture-driven tests.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

/// `fixtures/trades` in the repo.
pub fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/trades")
        .canonicalize()
        .expect("fixtures/trades must exist")
}

/// The user's real (never-committed) example folder, when it is present.
/// Tests that use it must skip cleanly when it is not.
pub fn real_root() -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../example_trades/Trades");
    path.canonicalize().ok().filter(|p| p.is_dir())
}

pub fn open_dir(root: &Path) -> PathBuf {
    root.join("Open Trades")
}

pub fn closed_dir(root: &Path) -> PathBuf {
    root.join("Closed Trades")
}

pub fn statements_dir(root: &Path) -> PathBuf {
    root.join("Daily Trade Statement")
}

/// Every order line in every trade file under `root`, as
/// `(file name, line number, line)`.
pub fn all_order_lines(root: &Path) -> Vec<(String, usize, String)> {
    let mut out = Vec::new();
    for dir in [open_dir(root), closed_dir(root)] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file())
            .filter(|p| {
                !p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
            })
            .collect();
        paths.sort();
        for path in paths {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&path).unwrap();
            for (i, line) in text.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                out.push((name.clone(), i + 1, line.to_string()));
            }
        }
    }
    out
}

/// Serialise for a snapshot with absolute paths reduced to file names, so the
/// goldens do not depend on where the repo lives.
pub fn snapshot_value<T: Serialize>(value: &T) -> Value {
    let mut v = serde_json::to_value(value).expect("serialisable");
    redact(&mut v);
    v
}

fn redact(value: &mut Value) {
    match value {
        Value::String(s) => {
            if s.contains('/') && (s.contains("fixtures") || s.contains("example_trades")) {
                if let Some(name) = Path::new(s.as_str()).file_name() {
                    *s = name.to_string_lossy().into_owned();
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact),
        Value::Object(map) => map.values_mut().for_each(redact),
        _ => {}
    }
}
