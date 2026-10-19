Принято — план по коду, не по концепциям. Дочитаю недостающие куски: вторую половину `run.rs` (`MutateOutcome`, `mutate_gate`), и стор/локи, которых ещё не видел.
Дочитал всё, что нужно: `run.rs` целиком (1714 строк, включая `MutateOutcome`/`MutReport`/`crap_gate`), `shell/store.rs`, `shell/lock.rs`, `core/state.rs` построчно. План ниже — только по фактуре кода. И начну с двух **реальных дефектов**, которые я нашёл при чтении — они и определяют порядок сессий.

## Два дефекта в твоём же коде (не теория)

**Дефект 1 — мёртвая ветка с ложной семантикой.** `run.rs:716-721`, в `gate_ladder`:
```rust
Verdict::SpecBlock { tail, .. } => {
    return Err(GateFail::Red { gate: Gate::Crap, tail, })
}
```
`crap_gate` сегодня возвращает только `Pass | EnvBlocked | Fail` — ветка мёртвая, написана лишь потому, что `match` по 4-вариантному `Verdict` обязан быть exhaustive. Но семантика её **нарушает твою же P7-таксономию**: SpecBlock = блокер спеки, `die(1)`, а не fix-раунд. Сегодня мёртвая — в тот день, когда какой-то гейт начнёт отдавать SpecBlock, она молча отправит его агенту чинить то, что агент чинить не может.

**Дефект 2 — claim не ходит через `state::transition`.** `run.rs:82-100`: при `prev_st != Active` код проверяет только WIP-лимит и делает `set_status(Active)`. Твой же тест `transition_rejects_shortcuts` запрещает `Done + Claimed` — а код пути это разрешает: `devh run W-003` на **done**-item молча реактивирует его. Стейт-машина есть, протестирована, и обойдена в единственном месте, где она нужна.

Оба дефекта — следствия ровно того, что чинят сессии 1 и 3. Это не «улучшение архитектуры ради красоты».

---

## Сессия 1 — `Verdict` везде, смерть четырёх enum'ов

**Спека (готова к `devh new`):**
```
Сделай: единая таксономия исходов гейтов. GateFail/LintKind/ReviewOutcome/MutateOutcome
в commands/run.rs заменяются на core::gates::Verdict (+носитель лога). Поведение
сохраняется, КРОМЕ: SpecBlock на уровне gate_ladder = die(1) «блокер спеки»
(сейчас мёртвая ветка уводит его в fix-раунд — нарушение P7).
OBSERVE:
- return: lint_gate/review_gate/crap_gate/mutate_gate -> (String, Verdict) или MutReport{verdict}
- state: enum LintKind/ReviewOutcome/MutateOutcome/GateFail исчезают из run.rs
- log: die-коды сохраняются: EnvBlocked -> rc=2, Fail -> fix-раунд или die(1), SpecBlock -> die(1)
RED: cargo test -p devh gate_failure_kind
RED-EXPECT: failure_kind_routes_spec_block
```

**Шаги (по текущим строкам):**

1. **Новая чистая функция** в `core/gates.rs` — роутинг вердикта в действие драйвера:
```rust
/// Класс реакции драйвера run() на вердикт гейта (P7-таксономия).
pub enum FailureKind { Fix, Env, Spec }
pub fn failure_kind(v: &Verdict) -> FailureKind {
    match v {
        Verdict::Fail { .. } => FailureKind::Fix,
        Verdict::EnvBlocked { .. } => FailureKind::Env,
        Verdict::SpecBlock { .. } => FailureKind::Spec,
        Verdict::Pass => unreachable!("failure_kind(Pass) — контракт вызова"),
    }
}
```
2. **Честный RED по твоей дисциплине**: сначала стаб — `pub fn failure_kind(_: &Verdict) -> FailureKind { FailureKind::Fix }`. Тест `failure_kind_routes_spec_block` падает **в рантайме с test-id** (не compile-error — твой же `judge_red_pre` честно считает компиляционный отказ SpecBlock'ом). Потом реализация → зелёный.
3. **Замена носителей** в `run.rs`:
   - `L548-551 enum GateFail` → `struct GateStop { verdict: Verdict, log: String }` (log — для Env-ветки `eprint!` перед `die`, как сейчас `GateFail::Env{log,message}`);
   - `L836-840 LintKind` + `lint_log_kind` → `lint_gate` возвращает `(String, Verdict)` напрямую. Заодно уходит stringly-typed детект `"lint env:"`-подстрокой из `lint_log_kind` — вердикт строится там, где факт известен;
   - `L914-918 ReviewOutcome` → `review_gate -> (String, Verdict)`: `Ok→Pass`, `Issues→Fail{gate: Review}`, `Env→EnvBlocked{missing}`;
   - `L964-968 MutateOutcome` → поле `MutReport.verdict: Verdict` (поля `fatal`/`early`/`stats` остаются — это данные отчёта, не вердикт);
   - `gate_ladder` (`L641-810`): все `return Err(GateFail::...)` → `GateStop`. Мёртвая ветка `L716-721` исчезает — SpecBlock уходит в общий match. `die(store,id,1)` внутри mutate-fatal (`L776-783`) остаётся как есть — это легитимная досрочная смерть.
4. **`run()` L315-344**: match по `stop.verdict` через `failure_kind`: `Fix` → раунд++, `Env` → `eprint!(stop.log); die(2)`, `Spec` → `die(1)` с текстом «блокер спеки».

**Приёмка:**
- `rg -c "enum (LintKind|ReviewOutcome|MutateOutcome|GateFail)" src/commands/run.rs` → 0;
- `bash devharness/test.sh` — все ассерты зелёные, **число ассертов не изменилось** (поведение сохранено);
- новые юниты: `failure_kind_routes_spec_block`, `failure_kind_routes_env` — с точными ассертами (не `is_ok()`);
- мутационный скоуп сессии: `core/gates.rs` + `commands/run.rs` (`--in-diff` сам ограничит);
- откат: один коммит, `git revert`.

**Цена:** полдня. **Что НЕ трогаем:** тексты сообщений, цвета, `die`-коды, порядок гейтов.

---

## Сессия 2 — стадии `run()` + чистые решения

Предусловие: сессия 1 смержена (драйверу нужен единый `Verdict`).

**Спека:**
```
Сделай: run() из ~500 строк -> драйвер ~80 строк + фазовые функции + чистая
decide_round(). Поведение и порядок стадий timing — БАЙТ-В-БАЙТ (devh ps парсит
timing.log). Все println!/die-семантика переносятся как есть.
OBSERVE:
- return: decide_round(round, max_fix, verdict) -> RoundDecision{Continue, GiveUp{...}}
- state: run() <= 100 строк; фазы: claim/ensure_worktree/red_pre/agent-loop/red_post/red_mutate/commit/finalize
- log: имена стадий timing.log неизменны: worktree(+bootstrap), red-pre, verify-rN, lint-rN, crap-rN, review-rN, mutate-rN, bench-rN, redmut, commit+push
RED: cargo test -p devh decide_round
RED-EXPECT: decide_round_gives_up_at_max_fix
```

**Разрез по твоим строкам:**

| Фрагмент | Куда | Примечание |
|---|---|---|
| `L50-77` (пrelude: проверки cfg, `ItemRunLock`, pid, агент-override) | остаётся в драйвере | **локи живут в драйвере** — `ItemRunLock`/`PidGuard`/`drop(lock)` на `L545` обязаны_span'ить весь run |
| `L78-100` claim | `fn claim()` | GlobalLock внутри, как сейчас |
| `L101-113` worktree | `fn ensure_worktree()` | |
| `L114-277` RED-pre цикл | `fn red_pre_phase() -> Result<RedPre>` | возвращает `enum RedPre { Proven{expect}, Resumed, Skipped }` — драйверу нужен только факт |
| `L292-345` агент-цикл | драйвер + **чистая** `decide_round` | единственная новая логика сессии |
| `L357-377`, `L378-449`, `L450-499`, `L500-547` | `red_post/red_mutate/commit/finalize` | перенос без изменений |

**Новая чистая функция:**
```rust
pub enum RoundDecision {
    Continue,
    GiveUp { code: i32 },   // 1 = гейт, 2 = env
}
pub fn decide_round(round: u32, max_fix: u32, kind: FailureKind) -> RoundDecision {
    match kind {
        FailureKind::Env => RoundDecision::GiveUp { code: 2 },
        FailureKind::Spec => RoundDecision::GiveUp { code: 1 },
        FailureKind::Fix if round < max_fix => RoundDecision::Continue,
        FailureKind::Fix => RoundDecision::GiveUp { code: 1 },
    }
}
```
RED: стаб `GiveUp{code:1}` всегда → тест `decide_round_gives_up_at_max_fix` (round=0, max=3, Fix → Continue) падает в рантайме → реализуй → зелёный.

**Контракт, который нельзя нарушить** (проверяется в приёмке): имена стадий `timing.stage(...)` — это парс-контракт `devh ps`; `set_status(Active)` в claim — единственный статус-райт сессии (L530 — см. сессию 3); `next_prompt` для fix-раунда приходит из `prompt::fix_prompt(&gate, &tail, ...)` — сигнатуры `prompt.rs` не трогаем.

**Приёмка:**
- `run()` ≤ 100 строк;
- до рефакторинга: прогнать одну smoke-сессию, сохранить `timing.log`; после: имена стадий идентичны (у тебя уже есть интеграционные сессии в `test.sh` — добавить ассерт на список стадий);
- test.sh зелёный без изменения ассертов;
- мутации: `decide_round` обязана убивать своих мутантов (граничный кейс `round == max_fix` — мутант `<` → `<=` обязан ловиться тестом — допиши кейс `decide_round(3, 3, Fix) == GiveUp`).

**Цена:** день. **Откат:** revert, тайминг-контракт сохранён = нет накопленного риска.

---

## Сессия 3 — `state.rs` до правды + фикс дефекта 2

Предусловие: сессия 2 смержена.

**Спека:**
```
Сделай: 1) claim идёт через state::transition: Done-item отклоняется с внятным
сообщением (сейчас run на done-item молча реактивирует — обход state-машины).
2) Store::set_status используется только через новый apply_event(id, event),
который читает статус -> transition() -> пишет. 3) events.log: append строкой
"ISO-дата | W-NNN | Event" на каждом переходе. 4) doctor валидирует событийный
лог: replay(events) == status файла (fold).
OBSERVE:
- return: claim_action(prev: Status) -> ClaimAction{Claim, Resume, RejectDone}
- return: fold(events: &[Event]) -> Status
- state: apply_event — единственный писатель items/W-NNN/status
- log: events.log растёт на Claimed/Merged/Aborted
RED: cargo test -p devh claim_action
RED-EXPECT: claim_action_rejects_done
```

**Шаги:**

1. **Чистая функция** (фикс дефекта 2):
```rust
pub enum ClaimAction { Claim, Resume, RejectDone }
pub fn claim_action(prev: Status) -> ClaimAction {
    match prev {
        Status::Open => ClaimAction::Claim,
        Status::Active => ClaimAction::Resume,
        Status::Done => ClaimAction::RejectDone,
    }
}
```
RED: стаб возвращает `Claim` всегда → `claim_action_rejects_done` падает → реализуй. В `claim()` сессии 2: `RejectDone` → `bail!("devh: W-NNN уже done — создай новый: devh new \"...\"")`.
2. **`Store::apply_event`** (`shell/store.rs`, рядом с `set_status`): читает `status_of` → `state::transition` → при `Ok` пишет статус + `append_raw("events.log", ...)`. `set_status` становится `pub(crate)`-private или удаляется из публичных — `rg -n "set_status" src/` должен показывать только `apply_event` (call-sites: `run.rs` claim → `Claimed`; `merge.rs:~126` → `Merged`; `misc::abort` → `Aborted`).
3. **`L530 set_status(Active)`** — удалить: no-op (claim уже поставил Active, ничего между ними статус не меняет). Красноречивая строка исчезает сама.
4. **`fold` + doctor**: `pub fn fold(events: &[Event]) -> Status` (Open + переходы). `doctor`: читает `events.log` для каждого item → `fold` → сверяет с `status`-файлом; расхождение = строка в отчёт (не смерть doctor'а). Это seed event-sourcing'а **без** смены source of truth: статус остаётся главным, events.log — проверяемая тень.

**Приёмка:**
- интеграция: `devh run <done-item>` → exit≠0, сообщение «уже done»;
- юниты: `claim_action_rejects_done`, `fold_replay_matches_transition` (последовательность `[Claimed, Merged] → Done`; `[Claimed, Aborted] → Open`);
- `rg -n "set_status" src/` — только внутри `apply_event`;
- test.sh зелёный; мутации на `state.rs` (твоя существующая зона `MUTATE_SCOPE`).

**Цена:** полдня-день.

---

## Сквозное (все три сессии)

- **Порядок жёсткий:** 1 → 2 → 3, каждая через merge до начала следующей. Сессия 2 без единого `Verdict` — двойная работа; сессия 3 без фаз — `claim()` ещё не существует как функция.
- **Оракул сохранения** — `test.sh` + 114 тестов: ассерты не редактируются ни в одной сессии (это твой же будущий test-freeze: дифф не трогает тестовые файлы, кроме добавления новых).
- **Мутации:** каждая сессия приносит 2–4 новые чистые функции — ровно тот класс, который твой гейт ловит лучше всего (`FnValue`-мутанты на `failure_kind`/`decide_round`/`claim_action` обязаны умереть).
- **Не делается в этих сессиях** (отдельные спеки потом): REVIEW-гейт второй моделью, P8-петля, bash-слой. Смешение = размывание RED.

Итого: три спеки, ~2–2.5 рабочих дня, два реальных бага закрыты, `run()` читаем, таксономия одна, стейт-машина — единственный путь изменения статуса. Начинать — с сессии 1: она полдня и разблокирует остальные. Если хочешь, следующий шаг — распишу spec.md для сессии 1 дословно (готовый текст под `devh new`, с red.cmd и RED-EXPECT), чтобы взять и запустить.
---

# План выполнения (детальный) — 2026-10-19, старт

Базовый документ выше (три сессии + два дефекта) сохранён без изменений. Ниже — план исполнения с якорями по **текущему** коду (номера строк на `4d3f5ae`, не по той ревизии, по которой писались сессии).

## 0. Базовая линия (замерено перед стартом)

- `cargo test -p devh`: **137 unit + 54 integration** — зелёные.
- `bash devharness/test.sh`: **PASS=127 FAIL=0**.
- `cargo fmt --all -- --check` чист; `cargo clippy -p devh --all-targets -- -D warnings` чист.
- Мутационный аудит P8 (AUDIT-2, `--in-diff 3765653..4fcedf1`): 155 мутантов — 147 caught / 8 unviable / **0 missed** (100% testable).

Якоря кода на `4d3f5ae` (сессии писались по другой ревизии — всё перемерено):

| Что | Где сейчас |
|---|---|
| `Verdict` (4 варианта) | `core/gates.rs:43-58` |
| `enum GateFail` | `commands/run.rs:584-588` |
| `gate_ladder -> Result<(), GateFail>` | `run.rs:~683-845`; мёртвая SpecBlock-ветка — в crap-match `~767-772` |
| `enum LintKind` + `lint_log_kind` (детект подстроки `"lint env:"`) | `run.rs:885-889`, `954-961` |
| `enum ReviewOutcome` + `review_gate` | `run.rs:966-970`, `972-1028` |
| `crap_gate` — уже `(String, Verdict)` | `run.rs:~1030` |
| `enum MutateOutcome` | `run.rs:1021-1025` |
| `MutReport.outcome` | `run.rs:1084` |
| сайты `MutateOutcome` | `run.rs`: 431/440/455 (run(), фаза red-mutation), 805/816/837 (gate_ladder), 1348-1587 (mutate_gate/finish/batch/per-file) |
| дефект 2: claim мимо `state::transition` | `run.rs:96-108` (`prev_st != Active` → WIP-чек → `set_status`) |
| мёртвый `set_status(Active)` в финале run() | `run.rs:565` |
| сайты `set_status` | `run.rs:104` (claim), `run.rs:565` (мёртвый), `merge.rs:166` (→Done), `misc.rs:223` (abort→Open), `worktree.rs:234` (тест) |

**Скоуп мутаций — грабля:** `devharness/cargo-mutants.toml` исключает `commands/**` — гейт не мутирует `run.rs` вообще. Аудиты сессий 1 и 2 делаем явным `-f devharness/src/commands/run.rs` (+ `--in-diff` для сужения к дельте сессии; иначе run.rs целиком = ~150+ мутантов на сессию).

## Протокол всех сессий

1. Воркет на сессию: `git worktree add /tmp/devh-a<N> -b devh/a<N>-<slug> main`. Главный чекаут не трогаем (грязный чужой параллельной сессией: ` D .cargo/mutants.toml`).
2. Оракул сохранения: `test.sh` (127) + `cargo test -p devh` (137+54) зелёные на каждом коммите; тестовые ассерты не правим — только добавляем.
3. RED-дисциплина: стаб → падение **в рантайме** → реализация → зелёный.
4. Мутационный аудит дельты сессии (`--in-diff <merge-base..HEAD>` + `-f run.rs` где нужно) — **0 missed** как условие приёмки.
5. Сдача: FF-merge ветки в main + push; main остаётся единственным HEAD'ом («всё объединить» — стоячая инструкция).

## Сессия 1 — единый `Verdict`, смерть четырёх enum'ов (детально)

**Цель:** `GateFail/LintKind/ReviewOutcome/MutateOutcome` исчезают из `run.rs`; `crap_gate`-образец `(String, Verdict)` становится единственным носителем; дефект 1 (мёртвая SpecBlock→fix-раунд) закрывается routing'ом в `run()`.

### 1.1 `failure_kind` (новая чистая функция, `core/gates.rs`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind { Fix, Env, Spec }

pub fn failure_kind(v: &Verdict) -> FailureKind {
    match v {
        Verdict::Fail { .. } => FailureKind::Fix,
        Verdict::EnvBlocked { .. } => FailureKind::Env,
        Verdict::SpecBlock { .. } => FailureKind::Spec,
        Verdict::Pass => unreachable!("failure_kind(Pass) — контракт вызова"),
    }
}
```

RED (`core/gates.rs` tests): `failure_kind_routes_spec_block` (SpecBlock→Spec), `failure_kind_routes_env` (EnvBlocked→Env), `failure_kind_routes_fix` (Fail→Fix) — стаб `FailureKind::Fix` роняет первые два **в рантайме**.

### 1.2 `GateStop` — носитель остановки гейта

```rust
struct GateStop { verdict: Verdict, log: String, message: String }
```

Отклонение от документа ({verdict, log}): `message` сохраняю полем — тексты гейтов различаются («lint env-блокер…», «CRAP env-блокер…», «RED-MUT env-блокер…»), вынос формулировок в `run()` = правка текстов = нарушение «поведение БАЙТ-В-БАЙТ». `log` — как сегодня (для `eprint!` перед `die`), `verdict` — роутинг + `gate`/`tail` для fix-раунда.

Две осознанные дельты поведения (обе — по спеке документа, ни одна не пиннится тестами — проверено: section16 ассертит текст «Блокер СРЕДЫ», не rc):
- **Env → `die(2)`** вместо `die(1)` — сходится к red-pre фазе, которая на env уже умирает 2;
- **SpecBlock → `die(1)`** «блокер спеки» вместо мёртвой уводящей в fix-раунд ветки (фикс дефекта 1). Если `crap_gate` начнёт отдавать SpecBlock (P7-taxonomy), человек чинит спеку, а не агент — fix-раунд.

### 1.3 Замена носителей (все в `run.rs`)

- `lint_gate(cfg, wt) -> (String, Verdict)`: env — `EnvBlocked{missing}`, fail — `Fail{gate: Lint, tail_of(log,30)}`, ok — `Pass`. `LintKind` + `lint_log_kind` удаляются; stringly-детект `"lint env:"` исчезает (вердикт строится там, где факт известен).
- `review_gate(...) -> (String, Verdict)`: `Ok→Pass`, `Issues→Fail{gate: Review, tail_of(log,5)}`, `Env(...)→EnvBlocked{missing}`.
- `crap_gate` без изменений — упрощается только match в `gate_ladder` (мёртвая SpecBlock-ветка уходит).
- `MutateOutcome` → поле `MutReport.verdict: Verdict`: `finish()` строит вердикт (Fail — kill-rate ниже порога; EnvBlocked — инструмент не распознан/блокер; Pass — ok/skip/0-мутантов/all-unviable); `fatal`/`early`/`stats` остаются данными отчёта. per-file-akkumulyator: `Env` → ранний возврат (как сегодня), первый `Fail` запоминается, `Pass` игнорируется.

### 1.4 `gate_ladder` и dispatch в `run()`

- все `return Err(GateFail::…)` → `return Err(GateStop{verdict, log, message})`;
- `Ok(())` остаётся; мутационный fatal (`die(1)` внутри gate_ladder) — как есть;
- dispatch в агент-цикле (`run.rs:~328`): `match gates::failure_kind(&stop.verdict)` → Fix: раунд++ и `fix_prompt` (как сегодня); Env: `eprint!(log); die(2)`; Spec: `die(1)` «блокер спеки».

### 1.5 Приёмка сессии 1

- `grep -cE 'enum (LintKind|ReviewOutcome|MutateOutcome|GateFail)' devharness/src/commands/run.rs` → 0;
- `bash devharness/test.sh` — PASS=127 FAIL=0 (ассерты не тронуты);
- `cargo test -p devh` — 137+54 + новые юниты `failure_kind_*`;
- fmt/clippy чистые;
- аудит: `cargo mutants --in-diff <main..HEAD diff> -f devharness/src/commands/run.rs -f devharness/src/core/gates.rs` — 0 missed;
- коммит `devh(A1): …`, FF-merge в main, push.

## Сессия 2 — разрез `run()` + чистый `decide_round` (детально)

Предусловие: сессия 1 в main. Разрез по текущим строкам (после сессии 1 сдвинутся — ремерять):

| Фрагмент (сейчас) | Куда |
|---|---|
| prelude: cfg-проверки, `ItemRunLock`, `write_pid`, `PidGuard`, agent-override (`run.rs:~611-660`) | остаётся в драйвере — локи обязаны span'ить весь run |
| claim (`~96-108`) + resume-worktree (`~110-135`) | `fn claim()` / `fn ensure_worktree()` |
| RED-pre цикл (`~199-293`) | `fn red_pre_phase() -> enum RedPre { Proven, Resumed, Skipped }` |
| env-диспатч + агент-цикл (`~294-354`) | драйвер + чистая `decide_round` |
| red-post / red-mutation / summary-commit-push / финализация (`~356-582`) | `red_post`, `red_mutate`, `commit_push`, `finalize` — перенос без изменений |

```rust
pub enum RoundDecision { Continue, GiveUp { code: i32 } }
pub fn decide_round(round: u32, max_fix: u32, kind: FailureKind) -> RoundDecision {
    match kind {
        FailureKind::Env => RoundDecision::GiveUp { code: 2 },
        FailureKind::Spec => RoundDecision::GiveUp { code: 1 },
        FailureKind::Fix if round < max_fix => RoundDecision::Continue,
        FailureKind::Fix => RoundDecision::GiveUp { code: 1 },
    }
}
```

RED: стаб `GiveUp{code:1}` → тесты `decide_round_gives_up_at_max_fix` (3,3,Fix → GiveUp), `decide_round_continues_below_max` (0,3,Fix → Continue), `decide_round_env_gives_two`, `decide_round_spec_gives_one` — падают в рантайме → реализация.

**Контракт, который нельзя нарушить:** имена стадий `timing.stage(...)` (`worktree(+bootstrap)`, `red-pre`, `verify-rN`, `lint-rN`, `crap-rN`, `review-rN`, `mutate-rN`, `bench-rN`, `redmut`, `commit+push`) — парс-контракт `devh ps`; переносим байт-в-байт, добавляем ассерт на список стадий. `run()` ≤ 100 строк — механический критерий приёмки. Возврат `gate_ladder` типизируется `GateStop` (уже после сессии 1).

Приёмка: run()≤100 строк; diff по `println!`/`die` — нулевой (сверяем выхлоп smoke-сессии до/после); test.sh 127; 137+54+новые; аудит `--in-diff -f run.rs` — 0 missed; коммит + FF-merge + push.

## Сессия 3 — `state.rs` до правды + фикс дефекта 2 (детально)

Предусловие: сессия 2 в main (claim стал функцией).

1. **`claim_action(prev: Status) -> ClaimAction{Claim, Resume, RejectDone}`** (чистая, `core/state.rs`): RED-стаб `Claim` → `claim_action_rejects_done`, `claim_action_resumes_active`, `claim_action_claims_open` → реализация. В `claim()`: `RejectDone` → `bail!("devh: W-NNN уже done — создай новый: devh new \"...\"")`; WIP-чек остаётся до claim (как сегодня).
2. **`Store::apply_event(id, event)`** (`shell/store.rs`, рядом с `set_status`): `status_of` → `state::transition` → при Ok — `atomic_write(status)` + `append_raw("events.log", "{ISO} | W-NNN | {event}")`. Call-sites переводим на него: `run.rs:104` claim (Claimed), `merge.rs:166` (Merged), `misc.rs:223` (Aborted). `set_status` становится `pub(crate)` — `grep -rn 'set_status' src/` показывает определение + `apply_event` (+ тест worktree.rs переводим на apply_event или оставляем pub(crate) с #[cfg(test)] — решить по ходу, тестовые ассерты не правим).
3. **`run.rs:565` мёртвый `set_status(Active)`** — удалить (claim уже поставил Active; между ними статус не меняется).
4. **`fold(events: &[Event]) -> Status`** + юнит `fold_replay_matches_transition` (`[Claimed, Merged]→Done; `[Claimed, Aborted]→Open`). Doctor: replay `events.log` каждого item → сверка с файлом `status`; расхождение = строка в отчёт (не смерть doctor'а).

Приёмка: интеграция `devh run <done-item>` → rc≠0 + «уже done»; `grep -rn 'set_status' devharness/src/` — только определение/`apply_event`; test.sh 127; 137+54+новые; аудит `--in-diff` на `core/state.rs` + `shell/store.rs` — 0 missed; коммит + FF-merge + push.

## Что НЕ делаем в этих сессиях

REVIEW-гейт второй моделью; P8-петля (закрыта); bash-слой. Смещение = размывание RED.

## Порядок исполнения

A1 (сейчас) → merge+push → A2 → merge+push → A3 → merge+push. Каждая сессия — один коммит-рефактор (+ возможность точечных фиксов в той же ветке до мержа).
