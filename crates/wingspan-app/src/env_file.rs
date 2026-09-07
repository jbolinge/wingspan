//! Minimal dotenv-style loader.
//!
//! Two layers use it: `./.env` in the working directory (read before the
//! config so `WINGSPAN_*` overrides in it take effect), and the external
//! secrets file named by `[schwab] env_file` (the production machine keeps
//! `SCHWAB_APP_KEY`/`SCHWAB_APP_SECRET` in a file owned by another app).
//!
//! Semantics are the conventional ones: `KEY=VALUE` lines, `#` comments, an
//! optional `export ` prefix, single or double quotes stripped, and a
//! variable that is **already set in the real environment always wins** — a
//! file can supply defaults, never override.
//!
//! "`#` comments" means what `dotenvy` and `python-dotenv` mean by it, and
//! this matters for a file full of secrets: a whole line starting with `#` is
//! skipped, and on an unquoted value a `#` that **follows whitespace** ends
//! the value (`KEY=abc  # note` is `abc`). A `#` with no space before it is
//! an ordinary character (`KEY=ab#c` is `ab#c`), and quoting protects
//! everything inside the quotes (`KEY="a # b"` is `a # b`), with a comment
//! allowed after the closing quote. Getting this wrong is not a parse error
//! the user ever sees: it is a credential with a comment glued to it, and the
//! only symptom is the provider's own rejection sentence.
//!
//! The lexing lives in [`parse`], which is pure: it hands back what the file
//! said and touches nothing. [`load`] is that plus the export, and is the
//! **only** `set_var` left in the application — the desktop reads its Schwab
//! credentials with `parse` and passes the values along, so a secret never
//! enters the process environment at all.

use std::collections::BTreeMap;
use std::path::Path;

/// Parse `path` if it exists. Pure: nothing is exported.
///
/// `Ok(None)` means the file is absent; otherwise every `KEY=VALUE` the file
/// held, in key order, whether or not the variable is already set.
pub fn parse(path: &Path) -> std::io::Result<Option<BTreeMap<String, String>>> {
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)?;
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        out.insert(key.to_string(), unquote(value.trim()).to_string());
    }
    Ok(Some(out))
}

/// One right-hand side, minus its quotes and its trailing comment.
///
/// `value` is already trimmed. Order matters: quoting is decided first, so a
/// `#` inside quotes survives and a comment after the closing quote does not
/// leave the quotes behind.
fn unquote(value: &str) -> &str {
    if let Some(quote) = value.chars().next().filter(|c| *c == '"' || *c == '\'') {
        if let Some(close) = value[1..].find(quote).map(|i| i + 1) {
            let rest = value[close + 1..].trim_start();
            if rest.is_empty() || rest.starts_with('#') {
                return &value[1..close];
            }
        }
        // An unbalanced quote, or trailing text that is not a comment. A
        // malformed line is data, not an error, so keep the old rule rather
        // than guessing at what was meant.
        if value.len() >= 2 && value.ends_with(quote) {
            return &value[1..value.len() - 1];
        }
        return value;
    }
    // Unquoted: a `#` preceded by whitespace (or standing alone) starts a
    // comment; one inside a word does not, because it is a legal character in
    // a generated secret and quoting is the escape hatch for the rest.
    match value
        .char_indices()
        .find(|(i, c)| *c == '#' && (*i == 0 || value[..*i].ends_with(|c: char| c.is_whitespace())))
    {
        Some((i, _)) => value[..i].trim_end(),
        None => value,
    }
}

/// [`parse`], then export the variables not already set. CLI only.
///
/// Returns `Ok(None)` when the file is absent, otherwise the number of
/// variables actually set — the real environment always wins.
pub fn load(path: &Path) -> std::io::Result<Option<usize>> {
    let Some(pairs) = parse(path)? else {
        return Ok(None);
    };
    let mut set = 0;
    for (key, value) in pairs {
        if std::env::var_os(&key).is_none() {
            std::env::set_var(&key, &value);
            set += 1;
        }
    }
    Ok(Some(set))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(dir: &tempfile::TempDir, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.path().join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        assert_eq!(load(Path::new("/definitely/not/here.env")).unwrap(), None);
    }

    #[test]
    fn parses_comments_export_and_quotes() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            &dir,
            "a.env",
            "# comment\nexport WINGSPAN_TEST_DOTENV_A=plain\nWINGSPAN_TEST_DOTENV_B=\"quo ted\"\nWINGSPAN_TEST_DOTENV_C='single'\nnot a line\n",
        );
        assert_eq!(load(&path).unwrap(), Some(3));
        assert_eq!(std::env::var("WINGSPAN_TEST_DOTENV_A").unwrap(), "plain");
        assert_eq!(std::env::var("WINGSPAN_TEST_DOTENV_B").unwrap(), "quo ted");
        assert_eq!(std::env::var("WINGSPAN_TEST_DOTENV_C").unwrap(), "single");
    }

    #[test]
    fn the_real_environment_always_wins() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("WINGSPAN_TEST_DOTENV_WINS", "from-env");
        let path = write(&dir, "b.env", "WINGSPAN_TEST_DOTENV_WINS=from-file\n");
        assert_eq!(load(&path).unwrap(), Some(0));
        assert_eq!(
            std::env::var("WINGSPAN_TEST_DOTENV_WINS").unwrap(),
            "from-env"
        );
    }

    /// The lexing rule a hand-written secrets file actually exercises.
    ///
    /// `.env.example` used to ship credential lines with a trailing note on
    /// them, and uncommenting one produced an app key with the note glued to
    /// it — a failed login whose only sentence is the provider's, naming
    /// nothing the user could act on. `parse`, not `load`: no process
    /// environment is touched.
    #[test]
    fn an_unquoted_trailing_comment_is_not_part_of_the_secret() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            &dir,
            "comments.env",
            concat!(
                "SPACED=abc # from my.tastytrade.com\n",
                "TABBED=abc\t# note\n",
                "EMPTY= # optional on the refresh grant\n",
                "QUOTED=\"ab#c\"\n",
                "QUOTED_THEN_COMMENT=\"a b\"  # note\n",
                "SINGLE_THEN_COMMENT='a b' # note\n",
                "HASH_IN_WORD=ab#c\n",
                "HASH_IN_QUOTES=\"ab #c\"\n",
            ),
        );
        let parsed = parse(&path).unwrap().expect("the file is there");
        let value = |key: &str| parsed.get(key).map(String::as_str);
        assert_eq!(value("SPACED"), Some("abc"));
        assert_eq!(value("TABBED"), Some("abc"));
        // Blank, which is what lets `providers` call it "not set" rather than
        // sending `# optional on the refresh grant` to a token endpoint.
        assert_eq!(value("EMPTY"), Some(""));
        assert_eq!(value("QUOTED"), Some("ab#c"));
        assert_eq!(value("QUOTED_THEN_COMMENT"), Some("a b"));
        assert_eq!(value("SINGLE_THEN_COMMENT"), Some("a b"));
        // `#` is a legal character in a generated secret: only whitespace
        // before it makes it a comment, and quoting covers the rest.
        assert_eq!(value("HASH_IN_WORD"), Some("ab#c"));
        assert_eq!(value("HASH_IN_QUOTES"), Some("ab #c"));
    }

    /// The seam the desktop needs: the credentials come back as values and
    /// the process environment is left exactly as it was.
    #[test]
    fn parse_reads_without_exporting() {
        const KEY: &str = "WINGSPAN_TEST_DOTENV_NEVER_EXPORTED";
        let dir = tempfile::tempdir().unwrap();
        let path = write(&dir, "c.env", &format!("{KEY}=a secret\n"));
        let parsed = parse(&path).unwrap().expect("the file is there");
        assert_eq!(parsed.get(KEY).map(String::as_str), Some("a secret"));
        assert!(
            std::env::var_os(KEY).is_none(),
            "`parse` exported {KEY} into the process environment"
        );
    }
}
