use std::path::{Path, PathBuf};
use std::process::Command;

fn git(cwd: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git").args(args).current_dir(cwd).output()?;
    if !out.status.success() {
        anyhow::bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_ok(cwd: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn git_out(cwd: &Path, args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

pub fn root_from_cwd() -> anyhow::Result<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    anyhow::ensure!(out.status.success(), "devh: не git-репозиторий");
    Ok(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

/// Главный worktree репозитория (где сделан основной checkout).
///
/// Внутри worktree агента `git rev-parse --show-toplevel` даёт корень
/// worktree, а `.harness/` (реестр оркестратора) живёт только в главном
/// дереве. Общий реестр/доска — обязательны, поэтому нужен общий корень:
/// `--git-common-dir` указывает на `.git` главного checkout'а.
pub fn main_root_from_cwd(wt: &Path) -> anyhow::Result<PathBuf> {
    let raw = git(wt, &["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .or_else(|_| git(wt, &["rev-parse", "--git-common-dir"]))?;
    let raw = raw.trim();
    anyhow::ensure!(!raw.is_empty(), "devh: git-common-dir пуст");
    let p = PathBuf::from(raw);
    let p = if p.is_absolute() { p } else { wt.join(p) };
    let p = p.canonicalize().unwrap_or(p);
    match p.file_name().and_then(|n| n.to_str()) {
        // `.git` главного checkout'а → его корень; linked worktree лежит
        // в `.git/worktrees/<имя>` — там главного дерева нет.
        Some(".git") => Ok(p.parent().map(Path::to_path_buf).unwrap_or_else(|| wt.to_path_buf())),
        _ => Ok(wt.to_path_buf()),
    }
}

pub fn has_remote(root: &Path) -> bool {
    !git_out(root, &["remote"]).trim().is_empty()
}

pub fn branch_current(root: &Path) -> Option<String> {
    let s = git_out(root, &["branch", "--show-current"]);
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

pub fn show_ref(root: &Path, branch: &str) -> bool {
    git_ok(
        root,
        &[
            "show-ref",
            "-q",
            "--verify",
            &format!("refs/heads/{branch}"),
        ],
    )
}

pub fn worktree_add_new(root: &Path, branch: &str, path: &Path) -> anyhow::Result<()> {
    git(
        root,
        &[
            "worktree",
            "add",
            "-b",
            branch,
            &path.to_string_lossy(),
            "HEAD",
        ],
    )
    .map(|_| ())
}

pub fn worktree_add_existing(root: &Path, branch: &str, path: &Path) -> anyhow::Result<()> {
    git(root, &["worktree", "add", &path.to_string_lossy(), branch]).map(|_| ())
}

pub fn worktree_remove(root: &Path, path: &Path) -> anyhow::Result<()> {
    git(
        root,
        &["worktree", "remove", "--force", &path.to_string_lossy()],
    )
    .map(|_| ())
}

/// Зарегистрированные worktree'ы: `git worktree list --porcelain` → пути.
pub fn worktree_paths(root: &Path) -> Vec<PathBuf> {
    git_out(root, &["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .map(PathBuf::from)
        .collect()
}

/// Сироты: git помнит путь, а на диске его нет (снято вручную/слесарками).
pub fn worktree_orphans(root: &Path) -> Vec<PathBuf> {
    worktree_paths(root)
        .into_iter()
        .filter(|p| !p.exists())
        .collect()
}

/// `git worktree prune` — снять регистрации пропавших путей.
pub fn worktree_prune(root: &Path) -> anyhow::Result<()> {
    git(root, &["worktree", "prune"]).map(|_| ())
}

pub fn branch_delete(root: &Path, branch: &str) -> anyhow::Result<()> {
    git(root, &["branch", "-D", branch]).map(|_| ())
}

/// Сбросить reuse-worktree на ветку задачи `branch`, сохранив `target/`.
/// Аварийные состояния прошлой задачи (краш посреди merge/rebase/cherry-pick)
/// снимаем молча — в общем пути это чужой мусор. Ветку задачи могло не быть
/// (reuse-путь помнит чужую) — тогда создаём от `base` (обычно main).
/// `clean -fd` БЕЗ `-x` выкидывает untracked-неигнорируемое, но цельно
/// оставляет gitignored `target/` — ради его тепла весь reuse и затевается.
pub fn reuse_checkout(wt: &Path, branch: &str, base: &str) -> anyhow::Result<()> {
    for st in ["merge", "rebase", "cherry-pick"] {
        let _ = git(wt, &[st, "--abort"]);
    }
    if git_ok(
        wt,
        &["show-ref", "--verify", &format!("refs/heads/{branch}")],
    ) {
        git(wt, &["checkout", "-f", branch])?;
    } else {
        git(wt, &["checkout", "-b", branch, base])?;
    }
    git(wt, &["reset", "--hard"])?;
    let _ = git(wt, &["clean", "-fd"]);
    Ok(())
}

/// Вернуть общий reuse-путь в нейтральное состояние (abort/clean завершённой
/// задачи): отсоединённый HEAD на base + сброс мусора. target/ цел — он и есть
/// смысл общего пути. Ветку base чекаутить нельзя (её держит главное дерево).
pub fn release_active(wt: &Path, base: &str) -> anyhow::Result<()> {
    git(wt, &["checkout", "--detach", base])?;
    git(wt, &["reset", "--hard"])?;
    let _ = git(wt, &["clean", "-fd"]);
    Ok(())
}

pub fn add_all(wt: &Path) -> anyhow::Result<()> {
    git(wt, &["add", "-A"]).map(|_| ())
}

/// Добавить в индекс ТОЛЬКО указанные пути (без `add -A`).
/// Путь-скоуп — основа песочницы `devh git`: ведомый коммитит только свои
/// файлы, чужие изменения в индекс не попадают.
pub fn add_paths(wt: &Path, paths: &[String]) -> anyhow::Result<()> {
    if paths.is_empty() {
        anyhow::bail!("git add: пустой список путей");
    }
    let mut args: Vec<&str> = vec!["add", "--"];
    args.extend(paths.iter().map(|p| p.as_str()));
    git(wt, &args).map(|_| ())
}

/// Файлы, уже лежащие в индексе (staged) — для проверки «в коммит попадут
/// только запрошенные пути».
pub fn staged_names(wt: &Path) -> Vec<String> {
    git_out(wt, &["diff", "--cached", "--name-only"])
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

pub fn has_staged(wt: &Path) -> bool {
    !git_ok(wt, &["diff", "--cached", "--quiet"])
}

pub fn staged_stat(wt: &Path) -> String {
    git_out(wt, &["diff", "--cached", "--stat"])
}

pub fn staged_deletions(wt: &Path) -> usize {
    git_out(wt, &["diff", "--cached", "--diff-filter=D", "--name-only"])
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count()
}

pub fn commit(wt: &Path, msg: &str) -> anyhow::Result<()> {
    git(wt, &["commit", "-q", "-m", msg]).map(|_| ())
}

pub fn push(wt: &Path, remote: &str, branch: &str) -> anyhow::Result<()> {
    git(wt, &["push", "-q", "-u", remote, branch]).map(|_| ())
}

pub fn push_main(wt: &Path, remote: &str, main: &str) -> anyhow::Result<()> {
    git(wt, &["push", "-q", remote, &format!("HEAD:{main}")]).map(|_| ())
}

pub fn push_delete(wt: &Path, remote: &str, branch: &str) -> bool {
    git_ok(wt, &["push", "-q", remote, "--delete", branch])
}

pub fn fetch(root: &Path, remote: &str, main: &str) -> anyhow::Result<()> {
    git(root, &["fetch", "-q", remote, main]).map(|_| ())
}

pub fn rebase(wt: &Path, onto: &str) -> anyhow::Result<()> {
    git(wt, &["rebase", onto]).map(|_| ())
}

pub fn fetch_dot_branch(root: &Path, branch: &str, main: &str) -> bool {
    git_ok(root, &["fetch", "-q", ".", &format!("{branch}:{main}")])
}

pub fn merge_ff_only(root: &Path, branch: &str) -> bool {
    git_ok(root, &["merge", "-q", "--ff-only", branch])
}

pub fn rev_list_count(wt: &Path, main: &str) -> anyhow::Result<u32> {
    let s = git(wt, &["rev-list", "--count", &format!("{main}..HEAD")])?;
    Ok(s.trim().parse().unwrap_or(0))
}

pub fn reset_hard_clean(wt: &Path) -> anyhow::Result<()> {
    git(wt, &["reset", "--hard", "HEAD"])?;
    git(wt, &["clean", "-fd"]).map(|_| ())
}

pub fn tree_dirty(wt: &Path) -> bool {
    !git_out(wt, &["status", "--porcelain"]).trim().is_empty()
}

pub fn diff_names(wt: &Path) -> Vec<String> {
    let out = git_out(wt, &["diff", "--name-only", "HEAD"]);
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.to_string())
        .collect()
}

pub fn status_names(wt: &Path) -> Vec<String> {
    git_out(wt, &["status", "--porcelain"])
        .lines()
        .filter_map(|l| {
            let name = l.get(3..)?.trim().to_string();
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

/// УСТАРЕВШИЙ fail-open путь (N1): ошибка git даёт пустой список, то есть
/// «диффа нет» там, где дерево не прочитано. Гейты (FREEZE, lint, контракт)
/// переведены на `changes_strict` в F-0.2; остаётся для тестов и вызовов,
/// где пустой список безобиден. Новым кодом не пользоваться.
pub fn changed_files(wt: &Path) -> Vec<String> {
    let mut all = diff_names(wt);
    all.extend(status_names(wt));
    all.sort();
    all.dedup();
    all
}

// ────────────────── F-0.1: строгий список изменений (N1, N5) ──────────────────
// Fail-closed: ошибка git или неизвестный статус = Err (вызывающий делает
// EnvBlocked), а НЕ пустой список. Прежний путь `git_out(...).unwrap_or_default()`
// давал fail-open: сбой git → «дифф пуст» → FREEZE проходит на грязном дереве.
// `-z` убирает кавычки вокруг не-ASCII путей, `--no-renames` + разворот
// rename в две записи закрывает обход глобов (`src/a.rs -> tests/b.rs`).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub kind: ChangeKind,
    pub path: String,
}

fn next_path<'a, I: Iterator<Item = &'a str>>(it: &mut I, st: &str) -> anyhow::Result<&'a str> {
    it.next()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| anyhow::anyhow!("нет пути после статуса {st:?}"))
}

/// Разбор вывода `git diff --name-status -z`. Тотальный и fail-closed:
/// любой неизвестный статус или обрыв потока — Err, а не «пусто».
/// Rename/copy разворачиваются в Deleted(old)+Added(new), даже если
/// `--no-renames` забыли (закрывает N5 на уровне парсера).
pub fn parse_name_status_z(raw: &str) -> anyhow::Result<Vec<Change>> {
    let mut out = Vec::new();
    let mut it = raw.split('\0').peekable();
    while let Some(st) = it.next() {
        if st.is_empty() {
            if it.peek().is_none() {
                break; // завершающий NUL
            }
            return Err(anyhow::anyhow!("пустой статус посреди потока"));
        }
        let mk = |kind: ChangeKind, p: &str| Change {
            kind,
            path: p.to_string(),
        };
        match st.as_bytes().first() {
            Some(b'A') => out.push(mk(ChangeKind::Added, next_path(&mut it, st)?)),
            Some(b'D') => out.push(mk(ChangeKind::Deleted, next_path(&mut it, st)?)),
            Some(b'M' | b'T' | b'U') => out.push(mk(ChangeKind::Modified, next_path(&mut it, st)?)),
            Some(b'R') => {
                let old = next_path(&mut it, st)?;
                let new = next_path(&mut it, st)?;
                out.push(mk(ChangeKind::Deleted, old));
                out.push(mk(ChangeKind::Added, new));
            }
            Some(b'C') => {
                let _src = next_path(&mut it, st)?;
                out.push(mk(ChangeKind::Added, next_path(&mut it, st)?));
            }
            _ => return Err(anyhow::anyhow!("неизвестный статус git: {st:?}")),
        }
    }
    Ok(out)
}

/// Строгие изменённые файлы worktree: tracked-diff + untracked.
/// Ошибка любого вызова git — Err (не пустой вектор).
pub fn changes_strict(wt: &Path) -> anyhow::Result<Vec<Change>> {
    let tracked = git(
        wt,
        &[
            "-c",
            "core.quotepath=off",
            "diff",
            "--name-status",
            "-z",
            "--no-renames",
            "HEAD",
        ],
    )?;
    let mut v = parse_name_status_z(&tracked)?;
    let untracked = git(wt, &["ls-files", "-z", "--others", "--exclude-standard"])?;
    for p in untracked.split('\0').filter(|p| !p.is_empty()) {
        v.push(Change {
            kind: ChangeKind::Added,
            path: p.to_string(),
        });
    }
    Ok(v)
}

fn names_scoped(wt: &Path, kind: &str, scope: &str) -> Vec<String> {
    let mut args: Vec<String> = if kind == "diff" {
        vec!["diff".into(), "--name-only".into(), "HEAD".into()]
    } else {
        vec!["status".into(), "--porcelain".into()]
    };
    args.push("--".into());
    for s in scope.split_whitespace() {
        args.push(s.to_string());
    }
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let out = git_out(wt, &refs);
    out.lines()
        .filter_map(|l| {
            let name = if kind == "diff" {
                l.trim().to_string()
            } else {
                l.get(3..)?.trim().to_string()
            };
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

/// Изменённые файлы, ограниченные scope (список git-pathspec через пробел).
/// Всегда объединение diff+status: `git diff HEAD` не видит неотслеживаемые
/// файлы, поэтому «status только если diff пуст» терял новые файлы агента
/// (мутанты для них не генерировались — гейт молча слабел).
/// Изменённые файлы в scope, СУЩЕСТВУЮЩИЕ на диске (delete/drop и т.п. отбрасываются).
pub fn changed_files_scoped(wt: &Path, scope: &str) -> Vec<String> {
    scoped_names(wt, scope)
        .into_iter()
        .filter(|f| wt.join(f).is_file())
        .collect()
}

/// То же, но БЕЗ отбрасывания удалённых: mutate-гейт должен сказать
/// «удалён агентом — мутации не требуется», а не делать вид, что файла нет.
pub fn changed_files_scoped_all(wt: &Path, scope: &str) -> Vec<String> {
    scoped_names(wt, scope)
}

fn scoped_names(wt: &Path, scope: &str) -> Vec<String> {
    let mut files = names_scoped(wt, "diff", scope);
    files.extend(names_scoped(wt, "status", scope));
    files.sort();
    files.dedup();
    files
}

/// Общий diff сессии (`git diff HEAD`) — ЕДИНЫЙ источник для ревьюера
/// (F-0.3) и будущего детектора ослабления тестов (F-2), чтобы их взгляды
/// на «что агент менял» не расходились.
///
/// Почему `git add -N .` перед `git diff HEAD`: без intent-to-add новые
/// файлы агента в `git diff HEAD` не видны, и они выпали бы из ревью —
/// а это самый ценный для гейта код. Побочный эффект безопасен:
/// intent-to-add только переводит untracked в staged-new, содержимое не
/// меняется (и на дайджест дерева не влияет — он читает файлы, не индекс).
///
/// Ошибка git → `Err` (fail-closed): вызывающий гейт превращает её в
/// `EnvBlocked`, а не в «диффа нет, пропуск».
pub fn diff_text(wt: &Path, scope: &str) -> anyhow::Result<String> {
    let _ = git_ok(wt, &["add", "-N", "."]);
    let mut args: Vec<&str> = vec!["diff", "HEAD"];
    let scopes: Vec<&str> = scope.split_whitespace().collect();
    if !scopes.is_empty() {
        args.push("--");
        args.extend(scopes.iter().copied());
    }
    let mut c = Command::new("git");
    c.args(&args).current_dir(wt);
    let o = c
        .output()
        .map_err(|e| anyhow::anyhow!("devh: git diff не запустился ({e})"))?;
    if !o.status.success() {
        anyhow::bail!(
            "git diff HEAD: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&o.stdout).to_string())
}

/// P7-2.2: diff для `--in-diff` cargo-mutants («только мутанты на строках,
/// затронутых этим diff'ом» — `-D, --in-diff <IN_DIFF>`). Пишем в
/// `.harness/mutate.diff` (вне git: `.git/info/exclude`, значит и вне
/// отпечатка kill-cache) и отдаём путь в шаблон как `{diff}`.
///
/// Взгляд на «изменённое» тот же, что у `changed_files_scoped_all`
/// (`git diff HEAD` + scope-pathspec) — скоуп `-f {files}` и скоуп
/// `--in-diff` не расходятся.
///
/// Пустой diff — не ошибка: cargo-mutants печатает «Diff file is empty»
/// и выходит 0 (0 мутантов), эту ветку гейт отличает от Env.
pub fn write_mutate_diff(wt: &Path, scope: &str, out: &Path) -> anyhow::Result<bool> {
    let text = diff_text(wt, scope)?;
    if let Some(par) = out.parent() {
        std::fs::create_dir_all(par).ok();
    }
    std::fs::write(out, &text)
        .map_err(|e| anyhow::anyhow!("devh: не записать {} ({e})", out.display()))?;
    Ok(!text.trim().is_empty())
}

pub fn current_branch(wt: &Path) -> Option<String> {
    let b = git_out(wt, &["rev-parse", "--abbrev-ref", "HEAD"]);
    (!b.trim().is_empty()).then(|| b.trim().to_string())
}

/// F-0.3: полный список файлов дерева worktree для дайджеста.
/// cached (`-c`) + untracked-not-ignored (`-o --exclude-standard`):
/// агент мог класть логику и в новый файл, и в игнорируемый путь — первое
/// видим, второе нет (граница модели, записано в rust_build.md).
pub fn tree_entries(wt: &Path) -> anyhow::Result<Vec<crate::core::digest::Entry>> {
    use crate::core::digest::{mode_from_permissions, parse_ls_files_z, Entry};
    use std::os::unix::fs::PermissionsExt;
    let mut c = Command::new("git");
    c.args(["ls-files", "-z", "-c", "-o", "--exclude-standard"])
        .current_dir(wt);
    let o = c
        .output()
        .map_err(|e| anyhow::anyhow!("devh: git ls-files не запустился ({e})"))?;
    if !o.status.success() {
        anyhow::bail!(
            "git ls-files: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    let mut out: Vec<Entry> = Vec::new();
    for rel in parse_ls_files_z(&String::from_utf8_lossy(&o.stdout)) {
        let p = wt.join(&rel);
        // symlink_metadata, не metadata: симлинк не разыменовывается, у него
        // свой режим 0o120000 и «содержимое» = цель.
        let meta = std::fs::symlink_metadata(&p)
            .map_err(|e| anyhow::anyhow!("дерево: не читать {} ({e})", p.display()))?;
        if meta.file_type().is_dir() {
            continue;
        }
        let (mode, content) = if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&p)
                .map_err(|e| anyhow::anyhow!("дерево: не читать ссылку {} ({e})", p.display()))?;
            (0o120000u32, target.to_string_lossy().as_bytes().to_vec())
        } else {
            let bytes = std::fs::read(&p)
                .map_err(|e| anyhow::anyhow!("дерево: не читать {} ({e})", p.display()))?;
            (mode_from_permissions(meta.permissions().mode()), bytes)
        };
        out.push(Entry {
            path: rel,
            mode,
            content,
        });
    }
    Ok(out)
}

/// F-0.3: sha256 дайджест дерева worktree (содержимое + режим + путь).
/// Сравнение до/после ревью: разошлось → ревьюер правил дерево.
pub fn tree_digest(wt: &Path) -> anyhow::Result<String> {
    let entries = tree_entries(wt)?;
    crate::core::digest::tree_digest(&entries).map_err(|e| anyhow::anyhow!("дайджест дерева: {e}"))
}

/// Unix-время последнего коммита в дереве (д�� сторожа простоя).
pub fn last_commit_secs(wt: &Path) -> Option<u64> {
    git_out(wt, &["log", "-1", "--format=%ct"])
        .trim()
        .parse::<u64>()
        .ok()
}

pub fn short_head(wt: &Path) -> String {
    git_out(wt, &["rev-parse", "--short", "HEAD"])
}

pub fn log_oneline(root: &Path, n: usize) -> String {
    let out = Command::new("git")
        .args(["log", "--oneline", &format!("-{n}")])
        .current_dir(root)
        .output()
        .ok();
    out.map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

/// Коммиты ветки, которых нет в базе (`git log base..HEAD`).
pub fn log_range(wt: &Path, base: &str, n: usize) -> anyhow::Result<String> {
    let out = Command::new("git")
        .args([
            "log",
            "--oneline",
            &format!("-{n}"),
            &format!("{base}..HEAD"),
        ])
        .current_dir(wt)
        .output()
        .map_err(|e| anyhow::anyhow!("devh: git log {base}..HEAD не запустился ({e})"))?;
    if !out.status.success() {
        anyhow::bail!(
            "git log {base}..HEAD: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Содержимое файла на ревизии (`git show <rev>:<path>`) — «что было до моей
/// правки» без чужого checkout.
pub fn show_file(wt: &Path, rev_path: &str) -> anyhow::Result<String> {
    let out = Command::new("git")
        .args(["show", rev_path])
        .current_dir(wt)
        .output()
        .map_err(|e| anyhow::anyhow!("devh: git show {rev_path} не запустился ({e})"))?;
    if !out.status.success() {
        anyhow::bail!(
            "git show {rev_path}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Диф/содержимое ревизии: `git show <rev> [-- <paths…>]`.
pub fn show_rev(wt: &Path, rev: &str, paths: &[String]) -> anyhow::Result<String> {
    let mut args: Vec<&str> = vec!["show", rev];
    if !paths.is_empty() {
        args.push("--");
        args.extend(paths.iter().map(|p| p.as_str()));
    }
    git(wt, &args)
}

/// Локальные ветки дерева (для агента: убедиться, что он в своей ветке).
pub fn branches(wt: &Path) -> Vec<String> {
    git_out(wt, &["branch", "--format=%(refname:short)"])
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// Откатить указанные пути к `HEAD` (ТОЛЬКО рабочее дерево, индекс не трогаем).
/// Вызывается фасадом `devh git restore --path …`: путь-скоуп + песочница
/// worktree делают операцию безопасной.
pub fn restore_paths(wt: &Path, paths: &[String]) -> anyhow::Result<()> {
    if paths.is_empty() {
        anyhow::bail!("git restore: пустой список путей");
    }
    let mut args: Vec<&str> = vec!["restore", "--source=HEAD", "--worktree", "--"];
    args.extend(paths.iter().map(|p| p.as_str()));
    git(wt, &args).map(|_| ())
}

/// Диф индекса относительно HEAD (`git diff --cached HEAD [-- paths]`).
pub fn diff_cached(wt: &Path, scope: &str) -> anyhow::Result<String> {
    let mut args: Vec<&str> = vec!["diff", "--cached", "HEAD"];
    let scopes: Vec<&str> = scope.split_whitespace().collect();
    if !scopes.is_empty() {
        args.push("--");
        args.extend(scopes.iter().copied());
    }
    git(wt, &args)
}

pub fn ls_remote_ok(root: &Path, remote: &str) -> bool {
    git_ok(root, &["ls-remote", "-q", remote, "HEAD"])
}

/// P7-2.3: файлы дерева с маркером cargo-mutants (`/* ~ changed by ... */`) —
/// остаётся, если прогон убит до revert'а. Только tracked-файлы: мутант
/// мутирует и так только исходники из индекса.
pub fn mutant_marked_files(wt: &Path) -> Vec<String> {
    git_out(wt, &["grep", "-l", "-F", crate::core::early::MARKER])
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// P7-2.3: ремонт после жёсткой остановки. Дерево в фазе гейта ГРЯЗНОЕ
/// (коммит идёт после лестницы), `git checkout` запрещён — единственный
/// честный путь: `git apply -R` свежих диффов из mutants.out/diff.
/// Дифф применяется только если его контекст совпал (то есть мутант ещё в
/// дереве), порядок — от свежих к старым (применён может быть последний).
/// Возвращает (отремонтированные, оставшиеся с маркером).
pub fn revert_mutant_diffs(wt: &Path, out_dir: &Path) -> (Vec<String>, Vec<String>) {
    let diff_dir = out_dir.join("diff");
    let mut diffs: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&diff_dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let p = e.path();
                    let m = e.metadata().ok()?;
                    Some((m.modified().ok()?, p))
                })
                .collect()
        })
        .unwrap_or_default();
    diffs.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    let mut repaired: Vec<String> = Vec::new();
    for (_, path) in diffs {
        if git_ok(wt, &["apply", "-R", "--check", &path.to_string_lossy()])
            && git_ok(wt, &["apply", "-R", &path.to_string_lossy()])
        {
            repaired.push(
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
            );
        }
    }
    let left = mutant_marked_files(wt);
    (repaired, left)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let git_ = |args: &[&str]| {
            let _ = git(d.path(), args);
        };
        git_(&["init", "-q"]);
        git_(&["config", "user.email", "t@example.com"]);
        git_(&["config", "user.name", "T"]);
        d
    }

    fn commit_all(d: &tempfile::TempDir) {
        let _ = git(d.path(), &["add", "-A"]);
        let _ = git(d.path(), &["commit", "-qm", "base"]);
    }

    #[test]
    fn mutant_markers_grep_and_repair_via_reverse_diff() {
        // P7-2.3: жёсткий SIGKILL оставил мутанта в tracked-файле;
        // ремонт = `git apply -R` свежего диффа (дерево грязное, checkout нельзя).
        let d = repo();
        let _ = std::fs::create_dir_all(d.path().join("src"));
        let _ = std::fs::write(d.path().join("src/lib.rs"), "fn a() { 1 }\n");
        commit_all(&d);
        let out_dir = d.path().join("mutants.out");
        let _ = std::fs::create_dir_all(out_dir.join("diff"));
        // «мутант»: поменяли тело + маркер, дифф сохранён ДО правки-ремонта
        std::fs::write(
            d.path().join("src/lib.rs"),
            "fn a() { 2 } /* ~ changed by cargo-mutants ~ */\n",
        )
        .unwrap_or_else(|_| unreachable!());
        let mut c = Command::new("git");
        let o = c
            .args(["diff", "src/lib.rs"])
            .current_dir(d.path())
            .output()
            .unwrap_or_else(|_| unreachable!());
        std::fs::write(
            out_dir.join("diff/x_line_1_col_1.diff"),
            String::from_utf8_lossy(&o.stdout).as_bytes(),
        )
        .unwrap_or_else(|_| unreachable!());
        assert_eq!(
            mutant_marked_files(d.path()),
            vec!["src/lib.rs".to_string()]
        );
        let (done, left) = revert_mutant_diffs(d.path(), &out_dir);
        assert_eq!(done.len(), 1, "{done:?}");
        assert!(left.is_empty(), "маркер обязан уйти: {left:?}");
        assert_eq!(
            std::fs::read_to_string(d.path().join("src/lib.rs")).unwrap_or_default(),
            "fn a() { 1 }\n"
        );
    }
    #[test]
    fn scoped_glob_matches_files_inside_src() {
        let d = repo();
        let _ = std::fs::create_dir_all(d.path().join("crates/uwa-api/src/routes"));
        let _ = std::fs::create_dir_all(d.path().join("crates/uwa-api/tests"));
        let _ = std::fs::write(d.path().join("crates/uwa-api/src/routes/m.rs"), "1\n");
        let _ = std::fs::write(d.path().join("crates/uwa-api/tests/e2e.rs"), "1\n");
        commit_all(&d);
        let _ = std::fs::write(d.path().join("crates/uwa-api/src/routes/m.rs"), "2\n");
        let _ = std::fs::write(d.path().join("crates/uwa-api/tests/e2e.rs"), "2\n");
        let got = changed_files_scoped(d.path(), "crates/*/src/*");
        assert_eq!(
            got,
            vec!["crates/uwa-api/src/routes/m.rs".to_string()],
            "{got:?}"
        );
    }

    #[test]
    fn scoped_bare_dir_glob_matches_nothing() {
        // git-fnmatch без FNM_PATHNAME требует совпадения ДО КОНЦА строки,
        // поэтому `crates/*/src` (без /*) не ловит файлы внутри src —
        // ровно этой ловушкой был сломан прежний MUTATE_SCOPE.
        let d = repo();
        let _ = std::fs::create_dir_all(d.path().join("crates/uwa-api/src"));
        let _ = std::fs::write(d.path().join("crates/uwa-api/src/lib.rs"), "1\n");
        commit_all(&d);
        let _ = std::fs::write(d.path().join("crates/uwa-api/src/lib.rs"), "2\n");
        assert!(changed_files_scoped(d.path(), "crates/*/src").is_empty());
        assert_eq!(
            changed_files_scoped(d.path(), "crates/*/src/*"),
            vec!["crates/uwa-api/src/lib.rs".to_string()]
        );
    }

    #[test]
    fn scoped_sees_untracked_new_files() {
        let d = repo();
        let _ = std::fs::create_dir_all(d.path().join("uwa-core/src"));
        let _ = std::fs::write(d.path().join("uwa-core/src/a.rs"), "1\n");
        commit_all(&d);
        // агент создал новый файл и НЕ staged его: только status его видит
        let _ = std::fs::write(d.path().join("uwa-core/src/new.rs"), "1\n");
        let got = changed_files_scoped(d.path(), "uwa-core/src/*");
        assert_eq!(got, vec!["uwa-core/src/new.rs".to_string()], "{got:?}");
    }

    #[test]
    fn scoped_empty_and_dot_match_all() {
        let d = repo();
        let _ = std::fs::write(d.path().join("a.txt"), "1\n");
        let _ = std::fs::write(d.path().join("b.txt"), "1\n");
        commit_all(&d);
        let _ = std::fs::write(d.path().join("a.txt"), "2\n");
        let _ = std::fs::write(d.path().join("b.txt"), "2\n");
        assert_eq!(changed_files_scoped(d.path(), "").len(), 2);
        assert_eq!(changed_files_scoped(d.path(), ".").len(), 2);
    }

    #[test]
    fn scoped_multiple_pathspecs_are_any_of() {
        let d = repo();
        for p in [
            "crates/uwa-api/src/a.rs",
            "crates/uwa-core/src/b.rs",
            "devharness/src/c.rs",
            "docs/d.md",
        ] {
            let path = d.path().join(p);
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
            let _ = std::fs::write(&path, "1\n");
        }
        commit_all(&d);
        for p in [
            "crates/uwa-api/src/a.rs",
            "crates/uwa-core/src/b.rs",
            "devharness/src/c.rs",
            "docs/d.md",
        ] {
            let _ = std::fs::write(d.path().join(p), "2\n");
        }
        let got = changed_files_scoped(d.path(), "crates/*/src/* crates/uwa-core/src/*");
        assert_eq!(
            got,
            vec![
                "crates/uwa-api/src/a.rs".to_string(),
                "crates/uwa-core/src/b.rs".to_string()
            ],
            "{got:?}"
        );
    }

    #[test]
    fn scoped_all_keeps_deleted_files_for_mutate_gate() {
        // mutate-гейт обязан сказать «удалён агентом — мутации не требуется»,
        // а не делать вид, что файла нет в изменённых (регрессия section18).
        let d = repo();
        let _ = std::fs::create_dir_all(d.path().join("uwa-core/src"));
        let _ = std::fs::write(d.path().join("uwa-core/src/a.rs"), "1\n");
        let _ = std::fs::write(d.path().join("uwa-core/src/b.rs"), "1\n");
        commit_all(&d);
        let _ = std::fs::remove_file(d.path().join("uwa-core/src/b.rs"));
        assert!(changed_files_scoped(d.path(), "uwa-core/src/*").is_empty());
        assert_eq!(
            changed_files_scoped_all(d.path(), "uwa-core/src/*"),
            vec!["uwa-core/src/b.rs".to_string()]
        );
    }

    #[test]
    fn restore_paths_reverts_only_given_file() {
        let d = repo();
        let _ = std::fs::write(d.path().join("a.txt"), "1\n");
        let _ = std::fs::write(d.path().join("b.txt"), "1\n");
        commit_all(&d);
        let _ = std::fs::write(d.path().join("a.txt"), "2\n");
        let _ = std::fs::write(d.path().join("b.txt"), "2\n");
        restore_paths(d.path(), &["a.txt".to_string()]).unwrap_or_else(|_| unreachable!());
        assert_eq!(
            std::fs::read_to_string(d.path().join("a.txt")).unwrap_or_default(),
            "1\n"
        );
        assert_eq!(
            std::fs::read_to_string(d.path().join("b.txt")).unwrap_or_default(),
            "2\n"
        );
    }

    #[test]
    fn show_file_and_diff_cached_read_history() {
        let d = repo();
        let _ = std::fs::write(d.path().join("a.txt"), "1\n");
        commit_all(&d);
        let _ = std::fs::write(d.path().join("a.txt"), "2\n");
        let _ = git(d.path(), &["add", "a.txt"]);
        let staged = diff_cached(d.path(), "").unwrap_or_else(|_| unreachable!());
        assert!(staged.contains("+2"), "staged diff: {staged}");
        let old = show_file(d.path(), "HEAD:a.txt").unwrap_or_else(|_| unreachable!());
        assert_eq!(old.trim(), "1");
        assert!(branches(d.path()).iter().any(|b| !b.is_empty()));
        assert!(log_range(d.path(), "HEAD", 5).is_ok());
    }

    #[test]
    fn current_branch_reports_head() {
        let d = repo();
        // файл обязателен: иначе commit_all падает («нечего коммитить») и
        // репо остаётся с unborn HEAD — rev-parse отдаёт «HEAD»
        let _ = std::fs::write(d.path().join("a.txt"), "1\n");
        commit_all(&d);
        let b = current_branch(d.path()).unwrap_or_default();
        assert!(!b.is_empty(), "ветка HEAD должна читаться");
        assert_ne!(b, "HEAD", "unborn/detached HEAD не есть ветка задачи");
    }
    #[test]
    fn write_mutate_diff_sees_tracked_and_new_files() {
        let d = repo();
        std::fs::write(d.path().join("src.rs"), "fn a() {}\n").unwrap_or_else(|_| unreachable!());
        commit_all(&d);
        let out = d.path().join(".harness").join("mutate.diff");
        // ничего не меняли — diff пустой, но файл существует (cargo-mutants сам скажет «empty»)
        assert!(
            !write_mutate_diff(d.path(), "src.rs", &out).unwrap_or_else(|_| unreachable!()),
            "без правок diff пуст"
        );
        assert!(out.is_file(), "файл диффа обязан существовать: {out:?}");
        // обычная правка + НОВЫЙ файл (intent-to-add)
        std::fs::write(d.path().join("src.rs"), "fn a() { let _ = 1; }\n")
            .unwrap_or_else(|_| unreachable!());
        std::fs::write(d.path().join("new.rs"), "fn b() {}\n").unwrap_or_else(|_| unreachable!());
        assert!(
            write_mutate_diff(d.path(), ".", &out).unwrap_or_else(|_| unreachable!()),
            "есть правки — diff непустой"
        );
        let text = std::fs::read_to_string(&out).unwrap_or_else(|_| unreachable!());
        assert!(text.contains("diff --git a/src.rs b/src.rs"), "{text}");
        assert!(
            text.contains("diff --git a/new.rs b/new.rs"),
            "новый файл агента обязан попадать в diff (иначе его мутанты вне скоупа): {text}"
        );
        // scope отсекает лишнее: diff только по src.rs
        let out2 = d.path().join(".harness").join("scoped.diff");
        write_mutate_diff(d.path(), "src.rs", &out2).unwrap_or_else(|_| unreachable!());
        let text2 = std::fs::read_to_string(&out2).unwrap_or_else(|_| unreachable!());
        assert!(!text2.contains("b/new.rs"), "scope не применился: {text2}");
    }
}

#[cfg(test)]
mod f0_gitparse_tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let _ = git(d.path(), &["init", "-q"]);
        let _ = git(d.path(), &["config", "user.email", "t@example.com"]);
        let _ = git(d.path(), &["config", "user.name", "T"]);
        d
    }

    #[test]
    fn f01_parse_basic_and_terminating_nul() {
        let v = parse_name_status_z("M\0a.rs\0A\0b.rs\0").unwrap_or_else(|_| unreachable!());
        assert_eq!(v.len(), 2);
        assert!(matches!(v.first(), Some(c) if c.kind == ChangeKind::Modified && c.path == "a.rs"));
        assert!(matches!(v.get(1), Some(c) if c.kind == ChangeKind::Added && c.path == "b.rs"));
    }

    #[test]
    fn f01_parse_rename_unfolds_even_without_flag() {
        let v = parse_name_status_z("R100\0src/old.rs\0tests/new.rs\0")
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(v.len(), 2);
        assert!(
            matches!(v.first(), Some(c) if c.kind == ChangeKind::Deleted && c.path == "src/old.rs")
        );
        assert!(
            matches!(v.get(1), Some(c) if c.kind == ChangeKind::Added && c.path == "tests/new.rs")
        );
    }

    #[test]
    fn f01_parse_unknown_status_is_err_not_empty() {
        assert!(parse_name_status_z("Z\0x.rs\0").is_err());
        assert!(parse_name_status_z("M\0").is_err());
        assert!(parse_name_status_z("M\0\0a.rs\0").is_err());
    }

    #[test]
    fn f01_parse_cyrillic_and_space_path_intact() {
        let v = parse_name_status_z("M\0src/я тут.rs\0").unwrap_or_else(|_| unreachable!());
        assert!(matches!(v.first(), Some(c) if c.path == "src/я тут.rs"));
    }

    #[test]
    fn f01_changes_strict_sees_all_three_kinds() {
        let d = repo();
        let _ = git(d.path(), &["add", "-A"]);
        std::fs::write(d.path().join("old.rs"), b"1").unwrap_or_else(|_| unreachable!());
        let _ = git(d.path(), &["add", "-A"]);
        let _ = git(d.path(), &["commit", "-qm", "base"]);
        std::fs::write(d.path().join("old.rs"), b"2").unwrap_or_else(|_| unreachable!());
        std::fs::write(d.path().join("new.rs"), b"x").unwrap_or_else(|_| unreachable!());
        let _ = git(d.path(), &["rm", "-q", "-f", "old.rs"]);
        let v = changes_strict(d.path()).unwrap_or_else(|_| unreachable!());
        assert!(v
            .iter()
            .any(|c| c.path == "old.rs" && c.kind == ChangeKind::Deleted));
        assert!(v
            .iter()
            .any(|c| c.path == "new.rs" && c.kind == ChangeKind::Added));
    }

    #[test]
    fn f01_changes_strict_non_repo_is_err_fail_closed() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        assert!(changes_strict(d.path()).is_err());
    }

    // ─── F-0.3: дайджест дерева и единый diff ───

    fn commit_here(d: &tempfile::TempDir) {
        let _ = git(d.path(), &["add", "-A"]);
        let _ = git(d.path(), &["commit", "-qm", "base"]);
    }

    #[test]
    fn f03_tree_digest_changes_on_edit_and_on_untracked() {
        let d = repo();
        std::fs::write(d.path().join("a.txt"), b"1").unwrap_or_else(|_| unreachable!());
        commit_here(&d);
        let base = tree_digest(d.path()).unwrap_or_else(|_| unreachable!());
        assert_eq!(
            base,
            tree_digest(d.path()).unwrap_or_else(|_| unreachable!()),
            "дайджест обязан быть стабилен на неизменном дереве"
        );
        // правка tracked
        std::fs::write(d.path().join("a.txt"), b"2").unwrap_or_else(|_| unreachable!());
        assert_ne!(
            base,
            tree_digest(d.path()).unwrap_or_else(|_| unreachable!())
        );
        // откат возвращает тот же дайджест
        std::fs::write(d.path().join("a.txt"), b"1").unwrap_or_else(|_| unreachable!());
        assert_eq!(
            base,
            tree_digest(d.path()).unwrap_or_else(|_| unreachable!())
        );
        // untracked-файл — тоже часть дерева (агент мог положить логику туда)
        std::fs::write(d.path().join("new.txt"), b"x").unwrap_or_else(|_| unreachable!());
        assert_ne!(
            base,
            tree_digest(d.path()).unwrap_or_else(|_| unreachable!())
        );
        // chmod +x меняет режим — без режима подмена невозможна
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(d.path().join("new.txt"))
                .unwrap_or_else(|_| unreachable!())
                .permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(d.path().join("new.txt"), perm)
                .unwrap_or_else(|_| unreachable!());
            assert_ne!(
                base,
                tree_digest(d.path()).unwrap_or_else(|_| unreachable!()),
                "chmod +x обязан менять дайджест"
            );
        }
    }

    #[test]
    fn f03_tree_digest_ignores_intent_to_add_index_noise() {
        // review_gate вызывает diff_text (тот делает `git add -N .`) ДО
        // дайджеста: если intent-to-add менял бы дайджест, честное ревью
        // стало бы env-блокером.
        let d = repo();
        std::fs::write(d.path().join("a.txt"), b"1").unwrap_or_else(|_| unreachable!());
        commit_here(&d);
        let before = tree_digest(d.path()).unwrap_or_else(|_| unreachable!());
        std::fs::write(d.path().join("new.txt"), b"x").unwrap_or_else(|_| unreachable!());
        let _ = git_ok(d.path(), &["add", "-N", "."]);
        let after = tree_digest(d.path()).unwrap_or_else(|_| unreachable!());
        assert_ne!(before, after, "новый файл виден (ls-files -o)");
        let text = diff_text(d.path(), "").unwrap_or_else(|_| unreachable!());
        assert!(text.contains("new.txt"), "untracked попал в diff: {text}");
        // повторный add -N тот же дайджест (дубль пути не валит дайджест)
        assert_eq!(
            after,
            tree_digest(d.path()).unwrap_or_else(|_| unreachable!()),
            "дубль пути из ls-files -c -o не должен ломать дайджест"
        );
    }

    #[test]
    fn f03_diff_text_non_repo_is_err_not_empty() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        assert!(diff_text(d.path(), "").is_err());
    }

    #[test]
    fn f03_diff_text_scoped_sees_only_scope() {
        let d = repo();
        std::fs::create_dir_all(d.path().join("src")).unwrap_or_else(|_| unreachable!());
        std::fs::write(d.path().join("src/a.rs"), b"1").unwrap_or_else(|_| unreachable!());
        std::fs::write(d.path().join("other.txt"), b"1").unwrap_or_else(|_| unreachable!());
        commit_here(&d);
        std::fs::write(d.path().join("src/a.rs"), b"2").unwrap_or_else(|_| unreachable!());
        std::fs::write(d.path().join("other.txt"), b"2").unwrap_or_else(|_| unreachable!());
        let scoped = diff_text(d.path(), "src/*").unwrap_or_else(|_| unreachable!());
        assert!(scoped.contains("src/a.rs"));
        assert!(!scoped.contains("other.txt"), "скоуп нарушен: {scoped}");
    }
}
