//! Guard: no file in the repository may carry a real account id.
//!
//! `scripts/anonymize_statements.py` rewrites every statement header
//! `Account Statement for <account-id>SCHW ...` so the id becomes the
//! placeholder `00000000TEST`, and `fixtures/trades/README.md` promises the
//! real number is replaced *everywhere*. This test enforces that promise over
//! the whole working tree - fixtures, sources, docs, snapshots and the
//! anonymiser itself - so a stray copy (a comment, a pasted sample, a new
//! golden) cannot re-introduce the value the script exists to strip.
//!
//! The real export lives outside the repo and is never scanned; failures are
//! reported by path and line only, so the offending value never reaches a log.

use std::path::{Path, PathBuf};

/// The literal that precedes the account id in a statement header.
const HEADER_MARKER: &str = "Account Statement for ";

/// The only account id allowed to appear in this repository.
/// Must match `REPLACEMENT_ACCOUNT` in `scripts/anonymize_statements.py`.
const REPLACEMENT_ACCOUNT: &str = "00000000TEST";

/// Directories whose contents are build output, dependencies or the git
/// object store rather than repository content.
const SKIPPED_DIRS: &[&str] = &[
    ".git",
    ".venv",
    "__pycache__",
    "dist",
    "node_modules",
    "playwright-report",
    "target",
    "test-results",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root must exist")
}

/// The 1-based line numbers in `text` whose statement header carries an
/// account id other than [`REPLACEMENT_ACCOUNT`].
///
/// A header written with a non-alphanumeric placeholder (`<account-id>`, `…`)
/// has no id to leak and is accepted.
fn offending_lines(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        for (at, _) in line.match_indices(HEADER_MARKER) {
            let rest = &line[at + HEADER_MARKER.len()..];
            let id: String = rest
                .chars()
                .take_while(char::is_ascii_alphanumeric)
                .collect();
            if !id.is_empty() && !id.starts_with(REPLACEMENT_ACCOUNT) {
                out.push(i + 1);
                break;
            }
        }
    }
    out
}

/// Every readable text file under `dir`, skipping [`SKIPPED_DIRS`].
fn text_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if !SKIPPED_DIRS.contains(&name.as_ref()) {
                text_files(&path, out);
            }
        } else if path.is_file() {
            out.push(path);
        }
    }
}

#[test]
fn no_file_in_the_repository_carries_a_real_account_id() {
    let root = repo_root();
    let mut files = Vec::new();
    text_files(&root, &mut files);
    assert!(
        files.len() > 50,
        "the repository walk found only {} files - it is not scanning the tree",
        files.len()
    );

    let mut offenders = Vec::new();
    for path in files {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        // Binary content cannot hold a header we care about.
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let relative = path.strip_prefix(&root).unwrap_or(&path);
        for line in offending_lines(&text) {
            offenders.push(format!("{}:{line}", relative.display()));
        }
    }

    assert!(
        offenders.is_empty(),
        "a real account id appears in {} place(s) - replace it with the \
         placeholder id or a `<account-id>` marker (the value itself is not \
         printed here on purpose):\n  {}",
        offenders.len(),
        offenders.join("\n  ")
    );
}

#[test]
fn the_guard_flags_a_real_id_and_accepts_the_redacted_one() {
    // Built at runtime so this file does not itself contain a header the walk
    // above would flag.
    let real = format!("{HEADER_MARKER}12345678SCHW (Individual) since 8/18/26");
    assert_eq!(offending_lines(&real), vec![1]);

    let redacted = format!("{HEADER_MARKER}{REPLACEMENT_ACCOUNT}SCHW (Individual) since 8/18/26");
    assert!(offending_lines(&redacted).is_empty());

    // A placeholder in prose or a comment has no id to leak.
    assert!(offending_lines(&format!("# {HEADER_MARKER}<account-id>SCHW ...")).is_empty());
    assert!(offending_lines(&format!("/// `{HEADER_MARKER}… since 8/18/26`")).is_empty());

    // The offending line is reported even when it is not the first.
    let multi = format!("ok\n{real}\nok\n{real}");
    assert_eq!(offending_lines(&multi), vec![2, 4]);
}

#[test]
fn the_anonymiser_still_declares_the_placeholder_this_guard_enforces() {
    let script = std::fs::read_to_string(repo_root().join("scripts/anonymize_statements.py"))
        .expect("the anonymiser must be present");
    assert!(
        script.contains(&format!("REPLACEMENT_ACCOUNT = \"{REPLACEMENT_ACCOUNT}\"")),
        "scripts/anonymize_statements.py no longer uses the placeholder id this guard enforces"
    );
}
