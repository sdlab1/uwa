//! Интеграционный сценарий оркестратора (T-132).
//!
//! Сквозной сценарий жизненного цикла задачи:
//!   board → list → start → dod → verify → done
//!
//! Гоняем настоящий бинарь `devh` с изолированным `DEVH_MSG_HOME`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn devh_bin() -> &'static str {
    env!("CARGO_BIN_EXE_devh")
}

fn setup_repo() -> (PathBuf, PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
    let home = dir.path().join("msg-home");
    let repo = dir.path().join("repo");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&repo).unwrap();
    let bare = dir.path().join("bare.git");
    run_git(None, &["init", "-q", "--bare", &bare.to_string_lossy()]);
    run_git(Some(&repo), &["init", "-q"]);
    run_git(
        Some(&repo),
        &["remote", "add", "origin", &bare.to_string_lossy()],
    );
    run_git(Some(&repo), &["config", "user.email", "t@e.com"]);
    run_git(Some(&repo), &["config", "user.name", "Test"]);
    fs::write(repo.join("README.md"), "# test\n").unwrap();
    run_git(Some(&repo), &["add", "."]);
    run_git(Some(&repo), &["commit", "-qm", "init"]);
    run_git(Some(&repo), &["branch", "-M", "main"]);
    run_git(Some(&repo), &["push", "-q", "-u", "origin", "main"]);
    (home, repo, dir)
}

fn run_git(cwd: Option<&PathBuf>, args: &[&str]) {
    let mut cmd = Command::new("git");
    if let Some(c) = cwd {
        cmd.current_dir(c);
    }
    let status = cmd.args(args).status().unwrap();
    assert!(status.success(), "git failed: {:?}", args);
}

fn devh(home: &PathBuf, cwd: &PathBuf, args: &[&str]) -> (bool, String) {
    let output = Command::new(devh_bin())
        .env("DEVH_MSG_HOME", home)
        .env("DEVH_REPO_ROOT", cwd)
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap_or_else(|_| unreachable!("devh не запускается"));
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (output.status.success(), format!("{stdout}{stderr}"))
}

/// Сценарий 1: board показывает структуру после init.
#[test]
fn scenario_board_after_init() {
    let (home, repo, _dir) = setup_repo();
    let (ok, _out) = devh(&home, &repo, &["init"]);
    assert!(ok, "devh init must succeed");
    let (ok, out) = devh(&home, &repo, &["orch", "board"]);
    assert!(ok, "devh orch board must succeed");
    assert!(
        out.contains("ДОСКА") || out.contains("board"),
        "board output missing structure: {out}"
    );
}

/// Сценарий 2: list показывает таблицу задач.
#[test]
fn scenario_list_shows_tasks() {
    let (home, repo, _dir) = setup_repo();
    let (ok, _out) = devh(&home, &repo, &["init"]);
    assert!(ok, "devh init failed");
    let (ok, out) = devh(&home, &repo, &["orch", "list"]);
    assert!(ok, "devh orch list must succeed");
    assert!(
        out.contains("ID") || out.contains("задач"),
        "list output missing task header: {out}"
    );
}

/// Сценарий 3: dod — переключение критериев DoD.
#[test]
fn scenario_dod_ack_cycle() {
    let (home, repo, _dir) = setup_repo();
    let (ok, _out) = devh(&home, &repo, &["init"]);
    assert!(ok, "devh init failed");

    // Добавляем задачу.
    let (ok, _out) = devh(
        &home,
        &repo,
        &[
            "orch",
            "add",
            "--agent",
            "ag-t1",
            "--p",
            "P2",
            "--dod",
            "unit тесты в модуле",
            "--check",
            "true",
            "--title",
            "Test task",
        ],
    );
    assert!(ok, "orch add failed");

    // DoD до подтверждения: 0/1.
    let (ok, out) = devh(&home, &repo, &["orch", "dod", "T-001"]);
    assert!(ok);
    assert!(
        out.contains("0/1") || out.contains("критерии 0/1"),
        "dod should show 0/1: {out}"
    );

    // Подтверждаем критерий.
    let (ok, out) = devh(
        &home,
        &repo,
        &["orch", "dod", "T-001", "--ack", "unit тесты в модуле"],
    );
    assert!(ok, "orch dod --ack failed: {out}");
    assert!(out.contains("1/1"), "dod should show 1/1: {out}");
}

/// Сценарий 4: verify запускает машинные проверки.
#[test]
fn scenario_verify_runs_check() {
    let (home, repo, _dir) = setup_repo();
    let (ok, _out) = devh(&home, &repo, &["init"]);
    assert!(ok, "devh init failed");

    let (ok, _out) = devh(
        &home,
        &repo,
        &[
            "orch",
            "add",
            "--agent",
            "ag-t2",
            "--p",
            "P2",
            "--dod",
            "проверка пройдена",
            "--check",
            "echo verify-ok",
            "--title",
            "Verify test",
        ],
    );
    assert!(ok, "orch add failed");

    // Подтвердим DoD.
    let (ok, _out) = devh(
        &home,
        &repo,
        &["orch", "dod", "T-001", "--ack", "проверка пройдена"],
    );
    assert!(ok);

    // Verify.
    let (ok, out) = devh(&home, &repo, &["orch", "verify", "T-001"]);
    assert!(ok, "orch verify failed: {out}");
    assert!(
        out.contains("verify-ok") || out.contains("✓"),
        "verify should show check passed: {out}"
    );
}
