//! Snapshot helpers for selector tests.
//!
//! Usage in tests:
//!
//! ```ignore
//! assert_html_snapshot!("chatgpt_basic", r#"<html>...</html>"#, "expected.txt", |html| {
//!     let ex = DomExtractor { /* ... */ };
//!     ex.extract_from_html(html).unwrap()
//! });
//! ```
//!
//! On mismatch, the actual output is written next to the expected file with a
//! `.actual` suffix — diff them and, if the change is desired, promote by
//! moving `.actual` over the expected file.

use std::path::{Path, PathBuf};

pub fn fixture_root() -> PathBuf {
    std::env::var_os("UWA_FIXTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"))
}

/// Assert that `extract(html)` equals the contents of `<fixtures>/<name>.txt`.
/// If the file doesn't exist yet, it is created (first-run bootstrap).
pub fn assert_snapshot(name: &str, html: &str, extract: impl Fn(&str) -> String) {
    let root = fixture_root();
    let html_path = root.join(format!("{name}.html"));
    let txt_path = root.join(format!("{name}.txt"));
    let actual = extract(html);

    // Persist input HTML for reproducibility.
    let _ = std::fs::create_dir_all(&root);
    if !html_path.exists() {
        let _ = std::fs::write(&html_path, html);
    }

    if !txt_path.exists() {
        std::fs::write(&txt_path, &actual).expect("write initial snapshot");
        return;
    }

    let expected = std::fs::read_to_string(&txt_path).expect("read snapshot");
    if expected == actual {
        return;
    }
    let actual_path = root.join(format!("{name}.txt.actual"));
    let _ = std::fs::write(&actual_path, &actual);
    panic!(
        "snapshot mismatch for `{name}`:\n  expected: {}\n  actual:   {}\n\
         diff: `diff -u {} {}`",
        txt_path.display(),
        actual_path.display(),
        txt_path.display(),
        actual_path.display(),
    );
}

/// Convenience for tests that want to iterate over a directory of HTML fixtures.
pub fn iter_html_fixtures(dir: impl AsRef<Path>) -> Vec<PathBuf> {
    let dir = dir.as_ref();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("html"))
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_and_match() {
        let tmp = std::env::temp_dir().join(format!("uwa_fix_{}", std::process::id()));
        std::env::set_var("UWA_FIXTURES", &tmp);
        let _ = std::fs::remove_dir_all(&tmp);
        assert_snapshot("t1", "<html><body>x</body></html>", |h| {
            h.contains('x').then(|| "x".to_string()).unwrap()
        });
        // Second call matches.
        assert_snapshot("t1", "<html><body>x</body></html>", |h| {
            h.contains('x').then(|| "x".to_string()).unwrap()
        });
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::remove_var("UWA_FIXTURES");
    }
}
