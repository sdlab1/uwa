//! `uwa update` — check GitHub Releases, download the archive matching the
//! compiled-in target, verify SHA256, and replace the running binary.
//!
//! Replacing the current binary while it runs is safe on Unix (unlink +
//! create) and on Windows (rename old, move new, delete old on next start).

use anyhow::{anyhow, bail, Context, Result};
use std::io::Read;
use std::path::PathBuf;

const REPO: &str = "sdlab1/uwa";
const CURRENT: &str = env!("UWA_VERSION");
const TARGET: &str = env!("UWA_TARGET");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckMode {
    Check,
    Apply,
}

pub async fn run(check_only: bool, force: bool) -> Result<()> {
    let mode = if check_only {
        CheckMode::Check
    } else {
        CheckMode::Apply
    };
    let latest = fetch_latest_tag()?;
    let latest_clean = latest.trim_start_matches('v');

    println!("current:  v{CURRENT}");
    println!("latest:   {latest}");

    if latest_clean == CURRENT && !force {
        println!("up to date");
        return Ok(());
    }
    if mode == CheckMode::Check {
        println!("run `uwa update` to install {latest}");
        return Ok(());
    }

    let archive = format!("uwa-{latest}-{TARGET}.tar.xz");
    let url = format!("https://github.com/{REPO}/releases/download/{latest}/{archive}");
    let sum_url = format!("{url}.sha256");

    let tmp = tempdir()?;
    let archive_path = tmp.join(&archive);
    let sum_path = tmp.join(format!("{archive}.sha256"));

    println!("→ downloading {archive}");
    download(&url, &archive_path)?;
    println!("→ verifying checksum");
    download(&sum_url, &sum_path).context("download checksum")?;
    verify_sha256(&archive_path, &sum_path)?;

    println!("→ extracting");
    extract_tar_xz(&archive_path, &tmp)?;

    let new_bin = tmp.join("uwa");
    if !new_bin.exists() {
        bail!("archive did not contain a `uwa` binary");
    }

    let current_exe = std::env::current_exe().context("current_exe")?;
    replace_binary(&current_exe, &new_bin)?;
    println!("✓ updated to {latest}");
    println!("  restart the daemon to pick it up:  uwa daemon stop && uwa daemon start");
    Ok(())
}

// ---------------------------------------------------------------------------
// GitHub lookup
// ---------------------------------------------------------------------------

fn fetch_latest_tag() -> Result<String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body = http_get_string(&url)?;
    // Minimal parse: the first `"tag_name"` field. Avoids a JSON dep
    // in the CLI. GitHub's API is stable for this one field.
    let after = body
        .split("\"tag_name\"")
        .nth(1)
        .ok_or_else(|| anyhow!("no tag_name in response"))?;
    let after = after
        .split(':')
        .nth(1)
        .ok_or_else(|| anyhow!("malformed tag_name"))?;
    let first_quote = after.find('"').ok_or_else(|| anyhow!("no opening quote"))?;
    let rest = &after[first_quote + 1..];
    let end = rest.find('"').ok_or_else(|| anyhow!("no closing quote"))?;
    Ok(rest[..end].to_string())
}

pub(crate) fn http_get_string(url: &str) -> Result<String> {
    let resp = ureq::get(url)
        .set("user-agent", concat!("uwa/", env!("UWA_VERSION")))
        .call()
        .with_context(|| format!("GET {url}"))?;
    resp.into_string().context("read response body")
}

fn download(url: &str, dest: &std::path::Path) -> Result<()> {
    let resp = ureq::get(url)
        .set("user-agent", concat!("uwa/", env!("UWA_VERSION")))
        .call()
        .with_context(|| format!("GET {url}"))?;
    let mut reader = resp.into_reader();
    let mut file =
        std::fs::File::create(dest).with_context(|| format!("create {}", dest.display()))?;
    std::io::copy(&mut reader, &mut file).with_context(|| format!("write {}", dest.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

fn verify_sha256(archive: &std::path::Path, sum_file: &std::path::Path) -> Result<()> {
    use sha2::{Digest, Sha256};

    let expected_raw = std::fs::read_to_string(sum_file)
        .with_context(|| format!("read {}", sum_file.display()))?;
    let expected = expected_raw
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow!("empty checksum file"))?
        .to_lowercase();

    let mut file =
        std::fs::File::open(archive).with_context(|| format!("open {}", archive.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let actual = hex(&hasher.finalize());
    if actual != expected {
        bail!("checksum mismatch: expected {expected}, got {actual}");
    }
    Ok(())
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

// ---------------------------------------------------------------------------
// Archive extraction (tar.xz only; that's what cargo-dist emits on Unix)
// ---------------------------------------------------------------------------

fn extract_tar_xz(archive: &std::path::Path, dest: &std::path::Path) -> Result<()> {
    let f = std::fs::File::open(archive)?;
    let xz = xz2::read::XzDecoder::new(f);
    let mut tar = tar::Archive::new(xz);
    tar.unpack(dest).context("extract tar.xz")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Binary replacement
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn replace_binary(current: &std::path::Path, new: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(new, std::fs::Permissions::from_mode(0o755))?;
    // A running binary can be renamed on Unix; the kernel keeps the inode.
    let tmp = current.with_extension("old");
    let _ = std::fs::remove_file(&tmp);
    std::fs::rename(current, &tmp).with_context(|| format!("move {} aside", current.display()))?;
    std::fs::rename(new, current)
        .with_context(|| format!("install new binary to {}", current.display()))?;
    let _ = std::fs::remove_file(&tmp);
    Ok(())
}

#[cfg(windows)]
fn replace_binary(current: &std::path::Path, new: &std::path::Path) -> Result<()> {
    // Windows: rename current to `.old`, move new into place. The `.old`
    // file is cleaned up on the next `uwa` start if it's still locked.
    let old = current.with_extension("old.exe");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(current, &old).with_context(|| format!("move {} aside", current.display()))?;
    std::fs::rename(new, current)
        .with_context(|| format!("install new binary to {}", current.display()))?;
    // `.old` may still be locked; the next start tries again.
    let _ = std::fs::remove_file(&old);
    Ok(())
}

fn tempdir() -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("uwa-update-{}", std::process::id()));
    if dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
