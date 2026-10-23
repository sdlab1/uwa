//! F-1: IO-обвязка Tripwire. Снимок защищённых файлов и refs до старта
//! агента, проверка после, откат из blob-копий. Решения — в core::tripwire.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::Digest;

use crate::core::tripwire::{
    self, Action, ArmDecision, ArmFile, CheckOutcome, Mode, Snapshot, Violation,
};
use crate::core::types::ItemId;
use crate::shell::store::{item_rel, Store};

const SKIP_WT: &[&str] = &[".git", "target", "node_modules"];
const SKIP_STORE: &[&str] = &[
    "mutate",
    "logs",
    "summaries",
    "msg",
    "worktrees",
    "lock",
    "tripwire",
    "tripwire.log",
    "log.md",
];

pub struct Armed {
    pub before: Snapshot,
    pub globs: Vec<String>,
    pub blobs: PathBuf,
    pub json_rel: PathBuf,
    pub wt: PathBuf,
    pub store_dir: PathBuf,
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = sha2::Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn git(cwd: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| anyhow::anyhow!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        anyhow::bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn refs_fingerprint(wt: &Path) -> anyhow::Result<String> {
    let raw = git(wt, &["for-each-ref", "--format=%(refname) %(objectname)"])?;
    Ok(sha256_hex(raw.as_bytes()))
}

fn collect_files(root: &Path, skip: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let rel = e.path().strip_prefix(root).unwrap_or(e.path());
            !skip.iter().any(|s| rel == Path::new(s) || rel.ends_with(s))
        })
        .flatten()
    {
        if entry.file_type().is_file() {
            out.push(entry.path().display().to_string());
        }
    }
    out
}

fn protected_globs(store: &Store, wt: &Path, id: &ItemId, extra: &[String]) -> Vec<String> {
    let mut out: Vec<String> = tripwire::PROTECTED_DEFAULTS
        .iter()
        .map(|p| wt.join(p).display().to_string())
        .collect();
    let item = store.item_dir(id);
    for f in [
        "spec.md",
        "red.cmd",
        "title",
        "files",
        "no-red",
        "no-crap",
        "no-files",
        "contract.toml",
        "allow-test-edit",
        "allow-freeze",
        "allow-protected",
    ] {
        out.push(item.join(f).display().to_string());
    }
    out.push(store.dir.join("config").display().to_string());
    for pat in extra {
        if !pat.trim().is_empty() {
            out.push(wt.join(pat.trim()).display().to_string());
        }
    }
    out
}

fn snapshot(
    wt: &Path,
    store_dir: &Path,
    globs: &[String],
    extra_keys: Option<&[String]>,
) -> anyhow::Result<Snapshot> {
    let mut files = collect_files(wt, SKIP_WT);
    files.extend(collect_files(store_dir, SKIP_STORE));
    let mut keys = tripwire::snapshot_keys(globs, &files).map_err(anyhow::Error::msg)?;
    if let Some(extra) = extra_keys {
        keys.extend(extra.iter().cloned());
    }
    let mut out = BTreeMap::new();
    for k in keys {
        let content = {
            let p = Path::new(&k);
            if p.is_file() {
                Some(sha256_hex(&std::fs::read(p)?))
            } else {
                None
            }
        };
        out.insert(k, content);
    }
    Ok(Snapshot {
        head: git(wt, &["rev-parse", "HEAD"])?,
        refs: refs_fingerprint(wt)?,
        files: out,
    })
}

fn classify(file: &Path) -> ArmFile {
    match std::fs::read_to_string(file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ArmFile::Missing,
        Err(_) => ArmFile::Corrupt,
        Ok(text) => match tripwire::snapshot_from_json(&text) {
            Ok(_) => ArmFile::Valid,
            Err(_) => ArmFile::Corrupt,
        },
    }
}

fn write_blobs(store: &Store, id: &ItemId, snap: &Snapshot) -> anyhow::Result<PathBuf> {
    let dir = store.item_dir(id).join("tripwire").join("blobs");
    std::fs::create_dir_all(&dir)?;
    for (path, sha) in &snap.files {
        let Some(sha) = sha else { continue };
        let bytes = std::fs::read(path)?;
        if sha256_hex(&bytes) != *sha {
            anyhow::bail!("tripwire: гонка при снимке {path} — содержимое изменилось");
        }
        let blob = dir.join(sha);
        if !blob.exists() {
            std::fs::write(&blob, &bytes)?;
        }
    }
    Ok(dir)
}

pub fn arm(store: &Store, wt: &Path, id: &ItemId, extra: &[String]) -> anyhow::Result<Armed> {
    let globs = protected_globs(store, wt, id, extra);
    let json_rel = item_rel(id, "tripwire.json");
    let parsed = store
        .read(&json_rel)
        .and_then(|t| tripwire::snapshot_from_json(&t).ok());
    let kind = classify(&store.dir.join(&json_rel));
    match tripwire::decide_arm(kind, parsed, false) {
        ArmDecision::ArmFresh => {
            let before = snapshot(wt, &store.dir, &globs, None)?;
            let blobs = write_blobs(store, id, &before)?;
            store.atomic_write(&json_rel, &tripwire::snapshot_to_json(&before))?;
            Ok(Armed {
                before,
                globs,
                blobs,
                json_rel,
                wt: wt.to_path_buf(),
                store_dir: store.dir.clone(),
            })
        }
        ArmDecision::Resume(before) => {
            let blobs = store.item_dir(id).join("tripwire").join("blobs");
            for sha in before.files.values().flatten() {
                if !blobs.join(sha).is_file() {
                    anyhow::bail!("tripwire: нет blob {sha} для resume");
                }
            }
            Ok(Armed {
                before,
                globs,
                blobs,
                json_rel,
                wt: wt.to_path_buf(),
                store_dir: store.dir.clone(),
            })
        }
        ArmDecision::Env(e) => anyhow::bail!("tripwire: {e}"),
    }
}

pub fn check(
    store: &Store,
    armed: &Armed,
    id: &ItemId,
    mode: Mode,
    incidents: u32,
) -> CheckOutcome {
    let extra: Vec<String> = armed.before.files.keys().cloned().collect();
    let after = match snapshot(&armed.wt, &armed.store_dir, &armed.globs, Some(&extra)) {
        Ok(s) => s,
        Err(e) => return CheckOutcome::Env(format!("снимок не прочитан: {e}")),
    };
    let allow_text = store
        .read(item_rel(id, "allow-protected"))
        .unwrap_or_default();
    let allow = tripwire::parse_allow(&allow_text);
    let found = tripwire::effective(tripwire::diff(&armed.before, &after), &allow);
    tripwire::decide_check(mode, found, incidents)
}

pub fn rollback(armed: &Armed, violations: &[Violation]) -> anyhow::Result<()> {
    let plan = tripwire::rollback_plan(&armed.before, violations);
    let mut manual_refs = false;
    for step in &plan {
        match step {
            Action::ResetMixed(sha) => {
                git(&armed.wt, &["reset", "--mixed", sha])?;
            }
            Action::Restore { path, sha } => {
                let bytes = std::fs::read(armed.blobs.join(sha))
                    .map_err(|e| anyhow::anyhow!("tripwire: blob {sha}: {e}"))?;
                if sha256_hex(&bytes) != *sha {
                    anyhow::bail!("tripwire: blob {sha} повреждён");
                }
                if let Some(parent) = Path::new(path).parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(path, &bytes)?;
            }
            Action::Remove(path) => {
                if Path::new(path).is_file() {
                    std::fs::remove_file(path)?;
                }
            }
            Action::ManualRefs => manual_refs = true,
            Action::Unrecoverable(path) => {
                anyhow::bail!("tripwire: нечего восстанавливать для {path}");
            }
        }
    }
    if manual_refs && refs_fingerprint(&armed.wt)? != armed.before.refs {
        anyhow::bail!("tripwire: refs изменены агентом — восстанови вручную");
    }
    let extra: Vec<String> = armed.before.files.keys().cloned().collect();
    let after = snapshot(&armed.wt, &armed.store_dir, &armed.globs, Some(&extra))?;
    tripwire::verify_rollback(&armed.before, &after).map_err(anyhow::Error::msg)
}

pub fn append_log(store: &Store, id: &ItemId, line: &str) -> std::io::Result<()> {
    store.append_raw("tripwire.log", &format!("{} | {line}\n", id.dir_name()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn run(cwd: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap_or_else(|_| unreachable!());
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn repo() -> (tempfile::TempDir, tempfile::TempDir) {
        let wt = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        run(wt.path(), &["init", "-q"]);
        std::fs::create_dir_all(wt.path().join("scripts")).unwrap_or_else(|_| unreachable!());
        std::fs::write(wt.path().join("scripts/verify.sh"), "#!/bin/sh\nexit 0\n")
            .unwrap_or_else(|_| unreachable!());
        std::fs::write(wt.path().join("rust-toolchain.toml"), "stable\n")
            .unwrap_or_else(|_| unreachable!());
        run(wt.path(), &["add", "."]);
        run(wt.path(), &["commit", "-q", "-m", "base"]);
        let root = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let store = Store::new(root.path());
        store
            .atomic_write("items/W-001/spec.md", "spec")
            .unwrap_or_else(|_| unreachable!());
        store
            .atomic_write("items/W-001/red.cmd", "true")
            .unwrap_or_else(|_| unreachable!());
        store
            .atomic_write("config", "VERIFY='true'\n")
            .unwrap_or_else(|_| unreachable!());
        (wt, root)
    }

    #[test]
    fn arm_check_rollback_restores_protected_inputs() {
        let (wt_dir, root) = repo();
        let wt = wt_dir.path();
        let store = Store::new(root.path());
        let id = ItemId(1);
        let armed = arm(&store, wt, &id, &[]).unwrap_or_else(|_| unreachable!());

        std::fs::write(wt.join("scripts/verify.sh"), "#!/bin/sh\nexit 1\n")
            .unwrap_or_else(|_| unreachable!());
        std::fs::remove_file(wt.join("rust-toolchain.toml")).unwrap_or_else(|_| unreachable!());
        std::fs::create_dir_all(wt.join("sub")).unwrap_or_else(|_| unreachable!());
        std::fs::write(wt.join("sub/cargo-mutants.toml"), "x\n").unwrap_or_else(|_| unreachable!());
        std::fs::write(wt.join("agent-work.txt"), "ok\n").unwrap_or_else(|_| unreachable!());
        run(wt, &["add", "."]);
        run(wt, &["commit", "-q", "-m", "agent"]);

        let out = check(&store, &armed, &id, Mode::Enforce, 0);
        let CheckOutcome::RollbackAndFail(v) = out else {
            unreachable!("ожидался RollbackAndFail, получено {out:?}")
        };
        assert!(v.len() >= 4, "{v:?}");
        if let Err(e) = rollback(&armed, &v) {
            unreachable!("rollback: {e:#}")
        }

        assert_eq!(
            std::fs::read_to_string(wt.join("scripts/verify.sh")).unwrap_or_default(),
            "#!/bin/sh\nexit 0\n"
        );
        assert!(wt.join("rust-toolchain.toml").is_file());
        assert!(!wt.join("sub/cargo-mutants.toml").exists());
        assert_eq!(
            check(&store, &armed, &id, Mode::Enforce, 1),
            CheckOutcome::Pass
        );
    }

    #[test]
    fn warn_does_not_rollback_and_refs_are_manual() {
        let (wt_dir, root) = repo();
        let wt = wt_dir.path();
        let store = Store::new(root.path());
        let id = ItemId(1);
        let armed = arm(&store, wt, &id, &[]).unwrap_or_else(|_| unreachable!());
        std::fs::write(wt.join("scripts/verify.sh"), "changed\n")
            .unwrap_or_else(|_| unreachable!());
        assert!(matches!(
            check(&store, &armed, &id, Mode::Warn, 0),
            CheckOutcome::Warn(_)
        ));
        run(wt, &["tag", "agent-tag"]);
        let out = check(&store, &armed, &id, Mode::Enforce, 0);
        let CheckOutcome::RollbackAndFail(v) = out else {
            unreachable!("ожидался RollbackAndFail")
        };
        assert!(v.iter().any(|x| matches!(x, Violation::RefsChanged)));
        assert!(rollback(&armed, &v).is_err());
    }

    #[test]
    fn arm_is_stable_on_resume_and_corrupt_is_env() {
        let (wt_dir, root) = repo();
        let wt = wt_dir.path();
        let store = Store::new(root.path());
        let id = ItemId(1);
        let a = arm(&store, wt, &id, &[]).unwrap_or_else(|_| unreachable!());
        let b = arm(&store, wt, &id, &[]).unwrap_or_else(|_| unreachable!());
        assert_eq!(a.before, b.before);
        store
            .atomic_write("items/W-001/tripwire.json", "{")
            .unwrap_or_else(|_| unreachable!());
        assert!(arm(&store, wt, &id, &[]).is_err());
    }
}
