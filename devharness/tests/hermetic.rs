//! ГЕРМЕТИЧНОСТЬ: devh test + cargo test не зависят от локальных артефактов.
//!
//! Тест проверяет, что в чистом окружении (пустой HOME, без .harness/config)
//! devh test выходит с понятным кодом ошибки (EnvError=2), а не с падением
//! «пустой реестр» — т.е. среда не готова обнаруживается на ранней стадии.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Изолированное окружение: пустой HOME, свой temp-каталог.
struct HermeticEnv {
    _dir: tempfile::TempDir,
    home: PathBuf,
    repo: PathBuf,
}

impl HermeticEnv {
    fn new() -> HermeticEnv {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&repo).unwrap();
        // devh требует git-репозиторий.
        let ok = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "git init в чистом окружении");
        HermeticEnv { _dir: dir, home, repo }
    }

    /// Запустить `devh test` в чистом окружении (пустой HOME, без .harness/config).
    fn run_devh_test(&self) -> (String, String, i32) {
        let output = Command::new(env!("CARGO_BIN_EXE_devh"))
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("DEVH_MSG_HOME", &self.home)
            .env("HARNESS_CONFIG", "")
            .arg("test")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        (stdout, stderr, output.status.code().unwrap_or(-1))
    }
}

/// devh test в окружении без .harness/config выходит с кодом 2 (EnvError),
/// а не падает с «пустой реестр» и не печатает «пустой реестр».
#[test]
fn devh_test_without_config_exits_env_error() {
    let env = HermeticEnv::new();
    let (stdout, stderr, code) = env.run_devh_test();
    // EnvError = 2 (среда не готова).
    assert_eq!(code, 2, "без .harness/config: код 2 (EnvError), а не {code}. stdout={stdout}, stderr={stderr}");
    // Ошибка должна быть понятной, а не «пустой реестр».
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        !combined.to_lowercase().contains("пустой реестр"),
        "не должно быть падения «пустой реестр» в чистой среде: {combined}"
    );
}
