//! Stages the SPA for `rust-embed`.
//!
//! `web/dist` is a build artefact and is git-ignored, so it is simply not
//! there in a fresh checkout — and `cargo clippy --all-features` (which CI
//! runs before the web job) would otherwise fail to compile with
//! "folder does not exist". Copying the SPA into `$OUT_DIR/ui`, and writing a
//! one-page placeholder when there is nothing to copy, means the `embed-ui`
//! feature always builds and the binary always answers `/` with something
//! honest.

use std::fs;
use std::path::{Path, PathBuf};

/// Served by an `embed-ui` build whose `web/dist` was never built.
const PLACEHOLDER: &str = r#"<!doctype html>
<meta charset="utf-8">
<title>Wingspan — UI not built</title>
<body style="font:15px/1.6 system-ui,sans-serif;max-width:40rem;margin:3rem auto;padding:0 1rem">
<h1>The SPA was not bundled into this binary</h1>
<p>This build has the <code>embed-ui</code> feature but <code>web/dist</code> was
empty when it was compiled. Build the UI first, then rebuild:</p>
<pre>cd web &amp;&amp; npm ci &amp;&amp; npm run build
cargo build --release --features embed-ui</pre>
<p>The API is live at <a href="/api/v1/health">/api/v1</a> either way.</p>
"#;

fn main() {
    let manifest =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let source = manifest.join("../../web/dist");
    let target = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("ui");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", source.display());

    // build-script cleanup of our own OUT_DIR copy; moves with the devserver in Phase 6
    #[allow(clippy::disallowed_methods)]
    let _ = fs::remove_dir_all(&target);
    fs::create_dir_all(&target).expect("create the staged UI directory");
    if source.is_dir() {
        copy_tree(&source, &target);
    }
    if !target.join("index.html").is_file() {
        fs::write(target.join("index.html"), PLACEHOLDER).expect("write the placeholder page");
    }
}

fn copy_tree(from: &Path, to: &Path) {
    let Ok(entries) = fs::read_dir(from) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let destination = to.join(entry.file_name());
        if path.is_dir() {
            let _ = fs::create_dir_all(&destination);
            copy_tree(&path, &destination);
        } else {
            println!("cargo:rerun-if-changed={}", path.display());
            let _ = fs::copy(&path, &destination);
        }
    }
}
