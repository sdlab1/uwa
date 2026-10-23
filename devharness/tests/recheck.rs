//! Интеграционный тест `devh mutate recheck` (B1).
//!
//! Главный баг читающей стороны: recheck читал сам `--output`, а настоящий
//! cargo-mutants пишет в `<--output>/mutants.out` — каталога не было, и
//! команда ВСЕГДА отвечала «исход не найден» (rc=2), фича не работала ни
//! разу. Тут проверяется реальный бинарь `devh` + реальный запуск
//! `cargo mutants` (со стабом вместо настоящего: R1/R11 — мутирующих
//! прогонов в тесте нет).
//!
//! `tests/integration.rs` в этой проверке НЕ используется: файл правит
//! другой агент (зоны не пересекаются).
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn devh_bin() -> &'static str {
    env!("CARGO_BIN_EXE_devh")
}

/// Стаб cargo-mutants: пишет то же, что настоящий.
///
/// * без `--output` → `./mutants.out`, с `--output X` → `X/mutants.out`
///   (иначе стаб врёт про то, где лежит прогон, и тест ничего не проверяет);
/// * `STUB_NO_OUTPUT=1` — выйти ДО создания каталога (диагностика
///   «инструмент не создал output»);
/// * `STUB_CAUGHT` / `STUB_MISSED` / `STUB_EXIT` — сценарий прогона.
const CARGO_MUTANTS_STUB: &str = r#"#!/usr/bin/env bash
base="."
prev=""
for a in "$@"; do
  if [ "$prev" = "--output" ]; then base="$a"; fi
  case "$a" in --output=*) base="${a#--output=}";; esac
  prev="$a"
done
[ -n "$STUB_NO_OUTPUT" ] && exit 66
out="$base/mutants.out"
mkdir -p "$out"
rm -f "$out"/*.txt "$out/outcomes.json" "$out/mutants.json"
caught="${STUB_CAUGHT:-0}"
missed="${STUB_MISSED:-0}"
succ="${STUB_SUCCEEDED:-0}"
total=$((caught + missed + succ))
for f in caught missed timeout unviable; do : > "$out/$f.txt"; done
printf '{"outcomes":[' > "$out/outcomes.part"
n=0
emit() {
  n=$((n + 1))
  f="devharness/src/core/plan.rs"
  name="$f:$n:5: replace x with None"
  if [ "$1" != "succeeded" ]; then printf '%s\n' "$name" >> "$out/$1.txt"; fi
  if [ "$n" -gt 1 ]; then printf ',' >> "$out/outcomes.part"; fi
  printf '{"scenario":{"Mutant":{"name":"%s","file":"%s","span":{"start":{"line":%d,"column":5}},"replacement":"None","genre":"FnValue"}},"summary":"%s","phase_results":[{"phase":"Test","duration":1.0}]}' "$name" "$f" "$n" "$2" >> "$out/outcomes.part"
}
i=0; while [ "$i" -lt "$caught" ]; do emit caught CaughtMutant; i=$((i + 1)); done
i=0; while [ "$i" -lt "$missed" ]; do emit missed MissedMutant; i=$((i + 1)); done
i=0; while [ "$i" -lt "$succ" ]; do emit succeeded Success; i=$((i + 1)); done
printf ']}' >> "$out/outcomes.part"
mv "$out/outcomes.part" "$out/outcomes.json"
printf '[\n]\n' > "$out/mutants.json"
echo "$total mutants tested in 1s"
exit "${STUB_EXIT:-0}"
"#;

/// Имя сценария — ровно то, что печатают `problems` и пишут append-списки.
const NAME: &str = "devharness/src/core/plan.rs:1:5: replace x with None";

struct Sandbox {
    _guard: tempfile::TempDir,
    root: PathBuf,
    stub_bin: PathBuf,
}

impl Sandbox {
    /// Корень с `.harness/mutate/state.json`: recheck берёт флаги прогона
    /// из `state.args` (иначе «аудит не запускался», rc=2). Супервизор не
    /// поднимается — тест детерминирован и не ждёт фоновых процессов.
    fn new() -> Self {
        let guard = tempfile::TempDir::new().unwrap_or_else(|_| unreachable!());
        let root = guard.path().join("recheck-root");
        let stub_bin = guard.path().join("stub-bin");
        fs::create_dir_all(root.join(".harness/mutate")).unwrap_or_else(|_| unreachable!());
        fs::create_dir_all(&stub_bin).unwrap_or_else(|_| unreachable!());
        // devh требует конфиг проекта (проверка при входе).
        fs::write(root.join(".harness/config"), "MIN_KILL=90\n").unwrap_or_else(|_| unreachable!());
        fs::write(
            root.join(".harness/mutate/state.json"),
            r#"{"args":["--test-tool","nextest"],"state":"done"}"#,
        )
        .unwrap_or_else(|_| unreachable!());
        // devh не работает вне git-репозитория (отпечаток дерева для overlay).
        let git = Command::new("git")
            .arg("init")
            .arg("-q")
            .arg(&root)
            .output()
            .unwrap_or_else(|_| unreachable!());
        assert!(
            git.status.success(),
            "git init: {}",
            String::from_utf8_lossy(&git.stderr)
        );
        write_exec(&stub_bin.join("cargo"), CARGO_MUTANTS_STUB);
        Self {
            _guard: guard,
            root,
            stub_bin,
        }
    }

    /// `devh <args>` со стабом cargo первым в PATH.
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> (i32, String) {
        let path = format!(
            "{}:{}",
            self.stub_bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new(devh_bin());
        cmd.args(args).current_dir(&self.root).env("PATH", path);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap_or_else(|_| unreachable!());
        let mut all = String::from_utf8_lossy(&out.stdout).to_string();
        all.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), all)
    }

    fn read_json(&self, rel: &str) -> serde_json::Value {
        let text = fs::read_to_string(self.root.join(rel)).unwrap_or_default();
        serde_json::from_str(&text).unwrap_or(serde_json::Value::Null)
    }
}

fn write_exec(p: &std::path::Path, content: &str) {
    use std::io::Write;
    let mut f = fs::File::create(p).unwrap_or_else(|_| unreachable!());
    f.write_all(content.as_bytes())
        .unwrap_or_else(|_| unreachable!());
    let mut perm = f
        .metadata()
        .unwrap_or_else(|_| unreachable!())
        .permissions();
    drop(f);
    use std::os::unix::fs::PermissionsExt;
    perm.set_mode(0o755);
    fs::set_permissions(p, perm).unwrap_or_else(|_| unreachable!());
}

/// Выживший: имя НАЙДЕНО в `<out>/mutants.out` → rc=1 и диагностика
/// «выжил», а не «исход не найден» (раньше — всегда rc=2).
#[test]
fn recheck_missed_mutant_is_found_in_out_muts_out() {
    let sb = Sandbox::new();
    let (rc, out) = sb.run(
        &["mutate", "recheck", NAME],
        &[("STUB_MISSED", "1"), ("STUB_EXIT", "2")],
    );
    assert_eq!(rc, 1, "выживший — recheck rc=1 (дыра): {out}");
    assert!(out.contains("✗ выжил"), "диагностика выжившего: {out}");
    assert!(out.contains(NAME), "имя мутанта в отчёте: {out}");
    assert!(!out.contains("исход для"), "НЕ «исход не найден»: {out}");
    assert!(
        !out.contains("не создал output"),
        "каталог <out>/mutants.out создан: {out}"
    );

    let v = sb.read_json(".harness/mutate/rechecked.json");
    assert_eq!(v.get("caught").and_then(|x| x.as_bool()), Some(false));
    assert_eq!(v.get("name").and_then(|x| x.as_str()), Some(NAME));
    // replacement/genre — ИЗ outcomes.json, а не пустые из append-списка.
    assert_eq!(v.get("replacement").and_then(|x| x.as_str()), Some("None"));
    assert_eq!(v.get("genre").and_then(|x| x.as_str()), Some("FnValue"));
}

/// Пойманный: в списки плохих он не попадает, только `caught.txt` →
/// rc=0 и overlay `caught=true` (раньше эта ветка была недостижима).
#[test]
fn recheck_caught_mutant_is_found_in_caught_txt() {
    let sb = Sandbox::new();
    let (rc, out) = sb.run(
        &["mutate", "recheck", NAME],
        &[("STUB_CAUGHT", "1"), ("STUB_EXIT", "0")],
    );
    assert_eq!(rc, 0, "пойман — recheck rc=0: {out}");
    assert!(out.contains("пойман"), "{out}");
    assert!(!out.contains("исход для"), "НЕ «исход не найден»: {out}");

    let v = sb.read_json(".harness/mutate/rechecked.json");
    assert_eq!(v.get("caught").and_then(|x| x.as_bool()), Some(true));
    assert_eq!(
        v.get("summary").and_then(|x| x.as_str()),
        Some("CaughtMutant")
    );
}

/// Инструмент не создал output → СВОЯ диагностика (не «исход не найден»),
/// rc=2 и код возврата инструмента в сообщении.
#[test]
fn recheck_without_output_catalog_says_tool_did_not_create_it() {
    let sb = Sandbox::new();
    let (rc, out) = sb.run(&["mutate", "recheck", NAME], &[("STUB_NO_OUTPUT", "1")]);
    assert_eq!(rc, 2, "нет каталога — rc=2: {out}");
    assert!(
        out.contains("инструмент не создал output"),
        "своя диагностика: {out}"
    );
    assert!(out.contains("rc Some(66)"), "код инструмента виден: {out}");
    assert!(
        !out.contains("исход для"),
        "«исход не найден» — не тут: {out}"
    );
}
