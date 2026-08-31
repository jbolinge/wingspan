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

use std::path::Path;

/// Load `path` if it exists. Returns `Ok(None)` when the file is absent,
/// otherwise the number of variables actually set.
pub fn load(path: &Path) -> std::io::Result<Option<usize>> {
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)?;
    let mut set = 0;
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
        let mut value = value.trim();
        if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            value = &value[1..value.len() - 1];
        }
        if std::env::var_os(key).is_none() {
            std::env::set_var(key, value);
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
}
