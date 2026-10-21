Сначала зафиксирую фактическое состояние после A1–A3 — смотрю новые `run.rs`, `gates.rs` (`FailureKind`/`GateStop`), `state.rs` (`claim_action`/`fold`), `store.rs` (`apply_event`) и ваш план-документ.
GitHub raw отдаёт закэшированное старое состояние — проверю по конкретному коммиту `642ab0b`, который ты назвал HEAD.
Проверил по коммиту `642ab0b` — всё честно: `run()` теперь строки 50–133 (~84 строки) + 12 фазовых функций (`require_gates` … `give_up`), `GateStop`/`RedInfo` на месте, `state.rs` несёт `claim_action` → `ClaimAction{Claim,Resume,RejectDone}` и `fold` с replay-семантикой для doctor. Основа для рывка реальна. Ниже — план серии **A4–A8 + B + C + D + K**, привязанный к этому коду. Всё, что не влезет, — в следующем сообщении.

---

# Серия A4–A8: доказанная оборона (docs/devh_advance.md, продолжение)

## A4 — Smoke-скелет: стаб-агенты + микрорепо + happy-path в CI

## A4 — СДАН (сессия a4-smoke, ветка devh/a4-smoke)

Секция test.sh = **31** (27..30 уже заняты). Реализация:
`devharness/tests/mk_smoke_repo.sh` (генератор: src/calc.sh с HEAD-дефектом +
tests/all.sh на диалекте cargo test + tools/{mutate,crap}.sh с режимами через
`.smoke/<tool>-mode` + полный config) и `devharness/stubs/good.sh` (happy-path
стаб + guard OBSERVE-инжекции: нет точки в промпте → rc=3). 7 калибровок к
черновику плана — rust_build.md §9.19 (главные: якорь `::` в test-id для
`failing_tests`; журнал событий per-item `.harness/items/W-NNN/events.log`;
agent.log содержит вывод, не промпт → proof инжекций маркером stdout;
`.harness/{items,summaries,logs}` нужны заранее — их создаёт init, а мы конфиг
пишем свой; секция сбрасывает `HARNESS_*` — env бьёт config в load_config).
Приёмка: test.sh 143 PASS / 0 FAIL (smoke-цикл ~1s), unit 155 + integration 55
зелёные (естественный прогон, load<4), fmt/clippy чисты, doctor smoke-репо
rc=0. Rust-кода A4 не касается — аудита мутантов нет.


## A5a — СДАН (сессия a5-adversarial, ветка devh/a5-adversarial)

Корпус атак v1, кейсы 1–4, 8 (читеры выхода агента). Секции test.sh = **32–36**,
хелперы `adv_run`/`adv_expect` (свежее микрорепо на итерацию, 3 итерации на
кейс), счётчики `ADV_OK`/`ADV_N` + строка «adversarial corpus: поймано N/N»
(метрика для B1). Таблица «чит → гейт → rc» и 5 калибровок — rust_build.md §9.20.
Дефект найдён корпусом в САМОМ гейте: `RED-EXPECT` матчил зелёную строку лога →
`judge_red_pre` мог принять «упал не по той причине» за честный RED. Фикс в
`gates.rs`: `is_green_line` + `failure_stream`, подпись матчится ТОЛЬКО по
потоку провала; SpecBlock получил хинт «подпись матчит ЗЕЛЁНЫЙ тест» (§9.20.1).
Приёмка: test.sh **234 PASS / 0 FAIL** (37 секций, корпус ~35s, kill-rate 15/15),
unit 158 + integration 55 зелёные (естественный прогон, load≈1.8), fmt/clippy
чисты, shellcheck 11 файлов; аудит мутантов на `gates.rs` (175 мутантов,
in-place + nextest + incremental) — итог: 175 мутантов, 170 caught / 0 missed / 5 unviable (kill-rate 100% testable), 65 мин in-place.


## A5b — СДАН (сессия a5b-gate-cheaters, ветка devh/a5b-gate-cheaters)

Корпус атак, кейсы 5–7 + 9–12 (читеры против ГЕЙТОВ). Секции test.sh = **37–43**:
NO-IGNORED (скрытый ignored:add_extra), LINT (настоящий shellcheck ловит SC2086
в изменённом .sh при зелёном VERIFY), MUTATE (kill-rate 80% < 90%, 2 fix-раунда),
MUTATE-env (агент снёс инструмент → rc=2 «mutation env-блокер», БЕЗ fix-раундов —
проверка таксономии env≠fail), CRAP (score=130 > 12), REVIEW («VERDICT: ISSUES»
каждый раунд → rc=1), REVIEW-env (молчание ревьюера → rc=2 «review env-блокер»).
`adv_expect` обобщён: 9-й аргумент `want_rc` (1/2), 10–11-й — лог-контроль
(`ctrl_file:ctrl_pat` в .harness/logs: GiveUp(Fix) печатает в stdout только имя
гейта — причина живёт в логе). Артефакты — rust_build.md §9.21.

**Итог: 36/36 поймано (kill-rate 100%) — ни одного дефекта гейта не найдено**
(в отличие от §9.20.1): все семь обязательств устояли без правки .rs. Первая
итерация стаба 37 дала ЛОЖНЫЙ ЗЕЛЁНЫЙ (worktree = чистый checkout, пустого
`.smoke/` git не хранит → `touch` молча падал) — поймано ручным прогоном,
исправление `mkdir -p "$wt/.smoke"` записано в §9.21 факт 1 (урок: атака должна
проверять, что её артефакт создан). Приёмка: test.sh **357 PASS / 0 FAIL**
(44 секции, корпус ~55s), unit 158 + integration 55 зелёные, fmt/clippy чистые,
shellcheck 18 файлов (+7 стабов). Rust-код не тронут — аудит мутантов не нужен.
Один флейк integration (31 failed на первом прогоне сразу после bash-сьюта —
contention в общем /tmp WT_ROOT); два ретрая 55/55 чисто — в журнал §9.21.

## A6 — СДАН (сессия a6-freeze, ветка devh/a6-freeze, коммит 597d448)

Дыра №1 закрыта: FREEZE-гейт — дифф сессии не выходит за заявленные файлы.
Первый в gate_ladder (дешевле всех: git-дифф до любого прогона, fail-fast).
Allowlist = секция спеки `FILES:` → items/W-NNN/files (глобы по строке) +
глобальный FREEZE_ALLOW (`*` только на концах: суффикс = каталог, префикс =
расширение). Правка tests/** — только через записанный обход
items/W-NNN/allow-test-edit с причиной (симметрия no-crap). Пустой allowlist
= пропуск С ЛОГОМ, не молчаливый Pass и не tripwire; doctor soft-warn при
FREEZE=1 и незаявленном scope у всех открытых спек. judge(Gate::Freeze) =
Fail: потеря гейта = красный (урок A1), а не тихий проход.

Механика: `freeze_violations`/`freeze_allowed`/`freeze_glob` — чистые функции
gates.rs (5 unit-тестов: hint на tests/**, записанный обход легитимирует
ТОЛЬКО tests/**, глобы + near-miss'ы `src2/`/`src`/`.mdx`, чистый дифф,
список ВСЕХ нарушений). `files_from_spec` (new.rs) — инлайн или буллеты до
следующей секции; 4 сайта создания item пишут items/W-NNN/files. run:
`freeze_gate` (allowlist + allow-test-edit → Fail с причинами в лог и tail;
первым в лестнице, timing.stage("freeze"), logs/W-NNN-freeze.log).
prompt fix: лечение = убрать лишнее из диффа или попросить человека записать
обход; сам ослаблять проверки запрещено. Конфиг: FREEZE (код-дефолт false,
шаблон init 1) + FREEZE_ALLOW; CONFIG_KEYS 34.

Корпус A5 получил 13-й кейс: freeze-cheat.sh (честно чинит HEAD-дефект И
ослабляет tests/all.sh — убирает add_non_numeric). Секции 44 (FREEZE=1 +
FILES: src/calc.sh → rc=1, лог-контроль «правка вне scope: tests/all.sh
(обход: allow-test-edit)»), 44b (freeze-off → rc=0 «OK гейты» — дыра
подтверждена как РЕАЛЬНАЯ: гейт не декорация), 44c (обход allow-test-edit →
rc=0 — FREEZE не tripwire, а allowlist). Стаб выживает на fix-раундах FREEZE
(маркер *FREEZE* в case, как стабы 37–43) — отрабатывает ровно max_fix=2.

**Итог: корпус 42/42 (kill-rate 100%); 13/13 по плану A6.** Приёмка:
test.sh 391 PASS / 0 FAIL (44 секции + 44b/44c), unit 164 + integration 55
зелёные, fmt/clippy чистые. Аудит мутантов (--in-diff, 43 мутанта): 34
caught / 9 unviable (8× `+=`→`-=`/`*=` в init.rs: String не импл
SubAssign/MulAssign; 1× fn-replace create_item: ItemId не Default) /
**0 missed** — вся freeze-поверхность поймана. Артефакты — rust_build.md §9.22.



**Ключевая идея, на которой стоит вся серия:** твои гейты парсят *выходные диалекты* инструментов (`"N mutants tested…: M caught"` из `parse_mutation_output`, `"CRAP: functions=N"` из `parse_crap_output`, `"VERDICT: …"` из `parse_review_verdict`). Значит, фейковые инструменты, говорящие на этих диалектах, прогоняют **настоящую** логику судей детерминированно и за секунды — без cargo-mutants (минуты) и без LLM (недетерминизм).

**Новые файлы:**

```
devharness/stubs/            # стаб-агенты (bash → автоматически под LINT-гейт shellcheck)
  good.sh                    # happy path
  red-green.sh  red-wrong.sh  no-summary.sh  bad-summary.sh  ignored-test.sh
  lint-dirty.sh  mutate-killer.sh  hang.sh  tool-gone.sh  crap-violation.sh
  review-ok.sh  review-issues.sh  review-mute.sh  freeze-cheat.sh  slow-mutate.sh
devharness/tests/mk_smoke_repo.sh   # генератор микрорепо во временный каталог
devharness/tests/chaos.sh           # A7
```

**`mk_smoke_repo.sh <dir> <stub-name>` — полное содержимое сути:**

```bash
#!/usr/bin/env bash
# Микрорепо для smoke/adversarial-сессий. Всё детерминировано, ноль LLM.
set -euo pipefail
DIR=$1; STUB=$2
git init -q -b main "$DIR"; cd "$DIR"
git config user.email t@t.local; git config user.name t
mkdir -p src tests tools .smoke

cat > src/calc.sh <<'EOF'
#!/usr/bin/env bash
add() {
  # FIXME(smk): нечисловой аргумент уходит в $(( )) молча — RED это ловит
  echo $(($1 + $2))
}
cmd=$1; shift
[ "$cmd" = add ] && add "$@"
EOF

cat > tests/all.sh <<'EOF'
#!/usr/bin/env bash
# VERIFY-цель; вывод — в диалекте cargo test (failing_tests/parse_ignored_names его парсят)
fail=0
out=$(bash src/calc.sh add 2 3)
if [ "$out" = 5 ]; then echo "test add_two ... ok"; else echo "test add_two ... FAILED"; fail=1; fi
out=$(bash src/calc.sh add x 3 2>&1) || true
case "$out" in *ERR*) echo "test add_non_numeric ... ok";; *) echo "test add_non_numeric ... FAILED"; fail=1;; esac
if [ -f .smoke/ignored ]; then echo "test add_extra ... ignored"; echo "2 passed, 1 ignored"
else echo "2 passed"; fi
exit $fail
EOF

cat > tools/mutate.sh <<'EOF'
#!/usr/bin/env bash
# Говорит на диалекте cargo-mutants 27 (gates::parse_mutation_output).
mode=$(cat .smoke/mutate-mode 2>/dev/null || echo ok)
case "$mode" in
  ok)      echo "5 mutants tested in 1s: 5 caught"; exit 0;;
  weak)    echo "5 mutants tested in 1s: 1 missed, 4 caught"; exit 2;;
  slow)    sleep 10; echo "5 mutants tested in 10s: 5 caught"; exit 0;;
  baseline) echo "cargo test failed in an unmutated tree"; exit 4;;
esac
EOF

cat > tools/crap.sh <<'EOF'
#!/usr/bin/env bash
# Диалект CRAP: "CRAP: functions=N" + строки "file:line fun C=n cov=n"
mode=$(cat .smoke/crap-mode 2>/dev/null || echo ok)
echo "CRAP: functions=2"
echo "src/calc.sh:3 add C=2 cov=90"
[ "$mode" = violation ] && echo "src/calc.sh:8 cmd C=15 cov=20"   # score>12 → Fail
echo "crap: 2 functions"
EOF

cat > .harness/config <<EOF
VERIFY=bash tests/all.sh
BOOTSTRAP=true
AGENT=bash $STUB
AGENT_TIMEOUT=60
MAX_FIX=2
MAX_ACTIVE=2
MUTATE=bash tools/mutate.sh
MUTATE_SCOPE=src
MIN_KILL=90
CRAP=bash tools/crap.sh
CRAP_SCOPE=src
CRAP_MAX=12
LINT=1
REVIEW=1
NO_IGNORED=1
MSG_DENY=Session [0-9]+
WT_ROOT=$TMPDIR
LOG_DAYS=7
EOF
git add -A && git commit -qm init
```

Заметь, что здесь уже встрроено: `red.cmd` в спеке = `bash tests/all.sh` — тот же раннер, что VERIFY → `red_links_verify` проходит; на чистом HEAD падает `add_non_numeric` → честный RED с test-id.

**`stubs/good.sh` (эталон, от которого наследуются читеры):**

```bash
#!/usr/bin/env bash
# Стаб-агент: prompt в $1, env HARNESS_WORKTREE/HARNESS_SUMMARY уже есть в agent_env().
set -euo pipefail
wt=${HARNESS_WORKTURE:-$HARNESS_WORKTREE}
# 1. «работа»: валидация аргументов add
cat > "$wt/src/calc.sh" <<'EOF'
#!/usr/bin/env bash
add() {
  case "$1$2" in ''|*[!0-9]*) echo "ERR: non-numeric" >&2; return 1;; esac
  echo $(($1 + $2))
}
cmd=$1; shift
[ "$cmd" = add ] && add "$@"
EOF
# 2. summary — обязательный (commit_push умирает без него)
printf 'fix: валидация нечисловых аргументов в add\n' > "$HARNESS_SUMMARY"
# 3. REVIEW-запрос? (тот же бинарник зовётся на ревью) — калибровочный маркер
case "$1" in *VERDICT*) echo "VERDICT: OK";; esac
exit 0
```

(Две калибровки при имплементации, укажи их в спеке: (1) реальный маркер review-промпта — посмотреть `prompt::review_prompt` иmatcher по нему, не по `VERDICT`; (2) env-имена точно из `agent_env()` — я читал их в run.rs: `HARNESS_WORKTREE`, `HARNESS_SUMMARY`.)

**Секция test.sh (A4, happy path):**

```bash
# === S27: SMOKE happy path через стаб-агента (<60s) ===
S=$(mk_smoke_repo "$(mktemp -d)" "$PWD/devharness/stubs/good.sh")
cd "$S"
printf 'Сделай: валидация нечисловых аргументов add.\nOBSERVE:\n- return: add x 3 -> ERR, rc=1\nRED: bash tests/all.sh\nRED-EXPECT: add_non_numeric\n' \
  | $DEVH new "smoke fix" >/dev/null
$DEVH run W-001 >/dev/null 2>&1; RC=$?
assert $RC -eq 0 "smoke: run обязан зелёный"
grep -q "claimed"  .harness/events.log   # A3-журнал живой
grep -q "merged"   .harness/events.log   # merge в той же сессии ниже
$DEVH merge W-001 >/dev/null 2>&1; assert $? -eq 0 "smoke: merge"
grep -q "add_non_numeric" .harness/logs/W-001-red.log
```

**Приёмка A4:** секция S27 зелёная в CI (`devharness-rs.yml` уже гоняет test.sh — новых job'ов не надо); полный цикл ≤ 60 с (замерить и вписать в `rust_build.md` §9.19); `devh doctor` на smoke-репо — rc=0.

**Цена:** одна сессия, ~3–4 ч.

## A5 — Adversarial-корпус v1: kill-rate гейтов

Каждый чит = стаб + секция test.sh + ассерт на **rc и маркер** (твоя таксономия rc: gate-fail/spec=1, env=2):

| # | Стаб (действие) | Обязан сработать | rc | Маркер для grep |
|---|---|---|---|---|
| 1 | `red-green.sh`: пишет red.cmd `exit 0` | RED pre: «не баг» | 1 | `уже зелёный\|не баг` |
| 2 | `red-wrong.sh`: фиксирует баг, но red.cmd падает другим тестом при `RED-EXPECT: add_non_numeric` | SpecBlock | 1 | `НЕ ПО ТОЙ ПРИЧИНЕ` |
| 3 | `no-summary.sh`: работа без summary | commit_push | 1 | `не написал summary` |
| 4 | `bad-summary.sh`: 1-я строка `Session 9: fix` | MSG_DENY | 1 | `гигиен` |
| 5 | `ignored-test.sh`: фиксирует + создаёт `.smoke/ignored` | NO-IGNORED | — fix-раунд | `незаявленные ignored: add_extra` |
| 6 | `lint-dirty.sh`: фикс + вносит SC2086 в `src/calc.sh` | LINT (реальный shellcheck) | — fix-раунд | `lint FAIL` |
| 7 | `mutate-killer.sh`: фикс + `.smoke/mutate-mode=weak` (80% < 90) | MUTATE → 2 fix-раунда → смерть | 1 | `kill-rate 80%` |
| 8 | `hang.sh`: `sleep 9999` (config `AGENT_TIMEOUT=60` → env `HARNESS_AGENT_TIMEOUT=5`) | watchdog | 1 | `FAIL агент`; elapsed < 30s |
| 9 | `tool-gone.sh`: правит конфиг `MUTATE=bash tools/nope.sh`… нет — агент конфиг не правит; проще: mk-скрипт с `MUTATE=bash tools/missing.sh` + good-стаб | EnvBlocked | **2** | `не распознан` |
| 10 | `crap-violation.sh`: фикс + `.smoke/crap-mode=violation` | CRAP | — fix-раунд | `CRAP.*score` / `crap VIOLATION` |
| 11 | `review-issues.sh`: на REVIEW-промпт печатает `VERDICT: ISSUES: не покрыт кейс x=0` | REVIEW | — fix-раунд | `ISSUES` |
| 12 | `review-mute.sh`: на REVIEW молчит | EnvBlocked review | **2** | `вердикт не распознан` |

Случай 9 честнее сделать не стабом, а вариантом mk-скрипта (агент не должен уметь править конфиг — и это отдельный будущий фриз). Помечу: env-override `HARNESS_MUTATE=bash tools/missing.sh` при запуске — твой существующий механизм `HARNESS_<KEY>`, даже репо не надо менять.

**Сессия A5 разбивается на две** (по 5–6 кейсов): A5a — читеры выхода агента (1–4, 8); A5b — читеры против гейтов (5–7, 9–12).

**Приёмка:** каждая секция детерминированна (3 прогона подряд — одинаковый rc); суммарно корпус ≤ 3 мин в CI; в `rust_build.md` — таблица «чит → гейт → rc». **Метрика появляется:** adversarial kill-rate = поймано/12. Публикуется в выводе test.sh одной строкой — это число пойдёт в B1 (уровень автономии).

**Документированная дыра №1 (не чинится в A5):** `freeze-cheat.sh` — правит `tests/all.sh` (ослабляет ассерт: убирает кейс add_non_numeric) → **сейчас проходит всё**. Его не добавляем в корпус как ожидающий — он станет RED-пруфом A6.

**Цена:** две сессии, ~6 ч суммарно.

## A6 — Freeze-гейт (test/scope-freeze): смерть дыры №1

**Спека (готовый текст):**
```
Сделай: гейт FREEZE — дифф сессии не выходит за заявленные файлы. Источник
allowlist: секция FILES: спеки → items/W-NNN/files (по строке-глобу) + глобальный
FREEZE_ALLOW. Правка тестовых файлов (tests/**) — только через записанный обход
items/W-NNN/allow-test-edit (симметрия no-crap). Гейт первый в gate_ladder —
дешевле всех, fail-fast до VERIFY. Конфиг: FREEZE (код: false, шаблон init: 1),
FREEZE_ALLOW. RED для гейта — стаб freeze-cheat: правит tests/all.sh без
allow-test-edit → сейчас всё зелёное, после — отклонение на FREEZE.
OBSERVE:
- return: freeze_violations(changed: &[String], allow: &[String], test_edit: bool) -> Vec<String>
- state: Gate::Freeze + judge-ветка; CONFIG_KEYS +2
- log: "FREEZE: правка вне scope: tests/all.sh (обход: allow-test-edit)"
RED: cargo test -p devh freeze_violations
RED-EXPECT: freeze_violations_flags_test_edit
```

**Чистая функция** в `core/gates.rs` — wildcard-матчер руками (~20 строк, `*`-суффикс/префикс/точное; он сам станет мутационной поверхностью, тесты обязательны точные). RED-порядок: стаб `FailureKind`-манер — сначала заглушка, тест `freeze_violations_flags_test_edit` падает в рантайме, потом реализация.

Позиция в `gate_ladder` (строки ~827–970): **первым**, до VERIFY. `git::changed_files(wt)` уже есть.

**Приёмка:** стаб `freeze-cheat.sh` (правит `tests/all.sh`, ассерт ослаблен) → rc=1, маркер `FREEZE`; с `allow-test-edit` (mk-скрипт кладёт файл с причиной) → проходит; корпус A5 получает 13-й кейс — kill-rate 13/13; мутации на `freeze_violations` — 0 missed (`--in-diff` сам ограничит скоуп).

**Цена:** одна сессия, ~3 ч.

## A7 — Chaos-e2e: kill -9 в случайной точке

**`tests/chaos.sh <iterations> [seed]`** (CI: 5 итераций; ночной workflow: 50):

```bash
for i in $(seq $ITERS); do
  RANDOM=$(( seed + i ))
  S=$(mk_smoke_repo "$(mktemp -d)" "$STUBS/slow-mutate-variant")  # фазы растянуты: sleep в good-стабах
  printf '...' | $DEVH new "chaos $i" >/dev/null
  ( cd "$S" && $DEVH run W-001 & echo $! > /tmp/chaos.pid )
  sleep $(( RANDOM % 8 ))            # 0..7с — размазано по стадиям
  kill -9 $(cat /tmp/chaos.pid) 2>/dev/null || true
  # — инварианты (функция check_invariants ниже) —
  ( cd "$S" && $DEVH doctor >/dev/null 2>&1 ); assert $? -eq 0 "chaos[$i]: doctor после kill -9"
  check_invariants "$S"               # см. ниже
  # — восстановление —
  ( cd "$S" && { $DEVH abort W-001 || $DEVH run W-001; } >/dev/null 2>&1 )
  assert $? -eq 0 "chaos[$i]: recovery (abort|resume)"
done
```

**`check_invariants`** — вот мясо, каждая строка ест твои же механизмы:
1. каждый `items/*/status` парсится (Open/Active/Done) — atomic_write гарантирует отсутствие torn-файлов;
2. `fold(events.log) == status` — **прямо твой A3-replay**, теперь под расстрелом;
3. нет `.harness/**/*.tmp` (осиротевшие атомарные записи);
4. в worktree нет мутационных маркеров (grep) — урок `ae9f619`;
5. `run.pid` отсутствует или процесс жив (stale → это находка doctor, не падение ассерта — фиксируй в лог итерации).

Лог итерации: `seed | sleep | стадия по timing.log | исход` — воспроизводимо. Для застревания в разных стадиях — вариант mk-скрипта со `sleep N` в стабе/инструментах (фазы red/verify/mutate).

**Приёмка:** 50 итераций ночью зелёные; любой красный → сид в `rust_build.md` + строка в RULES (твоя первая петля ест находку хаоса). CI: пятая итерация в `devharness-rs.yml` (+~1 мин).

**Цена:** одна сессия, ~3–4 ч.

## A7 — СДАН (сессия a7-chaos, ветка devh/a7-chaos, коммит по A7)

Chaos-e2e: `tests/chaos.sh <iterations> [seed]` убивает `devh run W-001`
сигналом -9 в случайной точке (SLP=((SEED+i*7)%8) сек, детерминированно по
сиду) и проверяет, что харнесс выживает и восстанавливается. Растяжка фаз:
SMOKE_SLOW=1 (mk_smoke_repo пишет 'slow' в .smoke/mutate-mode В КОММИТ —
worktree чистый checkout, файл вне коммита невидим) + стаб chaos-slow.sh
sleep 2 в агента → kill размазан по red → агент → verify → mutate.

**check_invariants (4 ассерта + 1 находка):** 1) status парсится
open/active/done (atomic_write, no torn); 2) fold(events.log)==status — A3-replay
под огнём (awk-реплика state::fold); 3) нет .harness/**/*.tmp сирот; 4) в
worktree нет мутационных маркеров (урок ae9f619); 5) stale run.pid — только
лог, находка doctor, не падение. Лог итерации: sleep/killed/rc/стадия ДО
recovery/doctor/recovery/inv — воспроизводимо по сиду.

**Грабли (все съедены, см. §9.23):** exec в под-оболочке (иначе kill бьёт
обёртку, devh-сирота держит run.lock); settle-петля pgrep на агента-сироту
перед doctor/abort/resume; STAGE читать до recovery (timing.log
перезаписывается); SC2002-useless-cat на чтении pid.

**Приёмка:** chaos 5 итер × 2 сида — 0 нарушений; test.sh секция 45 (3
итерации) PASS; bash-сьюта 393 PASS / 0 FAIL; CI devharness-rs.yml ×5
seed 2024; ночной devharness-chaos.yml (cron 03:00, dispatch до 50
итераций). cargo test standalone 164 unit + 55 integration зелёные (31/55
флейк сразу после bash-сьюты — контеншн /tmp WT_ROOT, §9.21). `.rs` не
тронуты — мутационный аудит не нужен.

## A8 — СДАН (сессия a8-flaky, ветка devh/a8-flaky)

Flaky-дисциплина v1: ключ `VERIFY_RETRIES` (дефолт 0) даёт РОВНО ОДИН
повтор VERIFY в раунде 1 — под `nice -n 19` + sleep 2. Прошёл со 2-го →
сессия живёт, НО: строка `дата | W-NNN | verify | round=1 | retried-ok` в
`.harness/flaky.md` + консоль `⚠ flaky-suspect (retry ok)` — молчаливого
«прошло» не существует. Молчаливое «прошло» — единственный враг: retry,
который нельзя отличить от чистого зелёного, обучает игнорировать
красное.

**Что вошло (код):** RunConfig.verify_retries + CONFIG_KEYS (35) +
init-шаблон в комментируемом блоке; gates::flaky_line / flaky_groups
(чистые, 6 unit-тестов); run.rs — verify_retry (отдельный лог
W-001-verify-retry.log, первый провал сохранён) + append_flaky +
ветвление в gate_ladder; doctor — состояние ключа + WARN с частотой
(группировка как у survivors). Stub flaky-agent.sh (good.sh без
OBSERVE-guard — иначе ломает честный fix-раунд).

**Семантика (3 угла, все проверены секцией 46):** 1) флейк + retries=1 →
rc=0, OK гейты, НО ledger + warning + разнесённые логи; 2) тот же флейк +
retries=0 → честный fix-раунд лечит, ledger пуст, warning нет — ключ
выключен, поведение как до A8; 3) настоящая поломка (ALWAYS) + retries=1
→ rc=1, `VERIFY красный`, ledger пуст. Ретраи ТОЛЬКО в круг 0: в
fix-раундах повтор маскировал бы эффект лечения.

**Модель флейка — ручка SMOKE_FLAKY в mk_smoke_repo.sh, а НЕ отдельный
verify-stub:** RED-LINK требует общего раннера у red.cmd и VERIFY —
отдельный флейковый verify-инструмент рвёт red-фазу (проверено). Пролог
tests/all.sh (счётный файл ВНЕ репо, в $T): первые два вызова красные,
дальше зелёный; ALWAYS-режим — красный всегда. Бонус: флейк идёт
настоящим run_logged-путём с настоящими логами.

**Приёмка:** unit 167 + integration 55 зелёные; bash-сьюта PASS=411
FAIL=0 (46a×2 / 46b / 46c); fmt/clippy чистые; --in-diff аудит = 40
мутантов, 31 caught / 0 missed / 9 unviable (kill-rate 100% testable).

**Ночной canary — готов в той же сессии:** `devharness-canary.yml`
(cron 04:00): полный oracle ×3 подряд с variance-детектом — детерминизм
со стабами означает побитово тот же `ИТОГО`, любое различие rc или
итога между прогонами = флаг среды (не кода: поломку ловит push-CI).
Плюс chaos ×50 (A7, seed 2024). Логи-артефакты при покраснении
(retention 14). Живого агента в canary нет — с реальным агентом это
ручной ритуал (devh bg auto на боевой сессии и есть canary). Проверен: YAML + bash -n.

## A8 — Flaky-дисциплина v1

Минимально честный контур (без ветки `FlakySuspect` в Verdict — не раздувай enum на v1):

- **Ключ `VERIFY_RETRIES` (дефолт 0):** при Fail VERIFY в раунде 1 — один повтор под `nice -n 19` + sleep 2 (твой же red-retry паттерн из `run_red`, обобщённый). Прошёл со второго раза → сессия продолжается, НО в `.harness/flaky.md` пишется строка `дата | W-NNN | verify | round=N | retried-ok` и в консоли `⚠ flaky-suspect (retry ok)`. Молчаливого «прошёл» не существует.
- **`devh doctor`** показывает `flaky.md` хвост + частоту (группировка как в survivors).
- **Ночной canary** — отдельный workflow `devharness-canary.yml`: chaos×50 + корпус×3 прогона подряд (детект variance: любые различия rc между прогонами = флаг). Реальный LLM-агент в CI не сажаем — он у тебя локальный; canary с живым агентом остаётся **ручным ритуалом** (`devh bg auto` на боевой сессии и есть canary, честно говоря).

**Цена:** ~2 ч (полсеccии, можно влить в A7-сессию хвостом).

---

# Серия B: побеги и автономия (docs/devh_autonomy.md)

## B1 — СДАН (ветка devh/b1-autonomy)

Ledger побегов + машинный уровень автономии. Побег = момент, где харнесс
не поймал то, что должен был (или человек починил мимо гейтов).
`.harness/escapes.md` правится РУКАМИ (как observations): строка формата
`дата | W-NNN | найдено: … | класс: blind:* | реакция: …`, побег без
конвертации (пусто/`-`) = debt. Тулинг только читает — `devh escapes`
(агрегация: группы классов, долг списком, streak, корпус, итоговая
строка), `--add` намеренно нет.

**Что вошло (код):** core/escapes.rs — `Escape`, `parse_escape_line`
(просеивающий: мусор/дата-не-дата/сессия-не-W/пустой класс → None;
4 поля без реакции = валидная строка-debt), `autonomy_level` — чистая:
любой долг → L0 ВСЕГДА; L1 = streak≥10 && kill≥1.0; L2 = streak≥30 &&
kill≥1.0; иначе L0. `streak_of` — заголовки `## W-… (ISO-дата)` из
log.md с датой строго после последнего побега (общая с survivors
механика date_in_parens; сессия без даты не считается).
`class_groups` (count desc, name asc — зеркало flaky_groups),
`corpus_kill`/`kill_rate` ({"caught":N,"total":N,"date":…}; total 0 →
0.0 — «нет данных» не даёт 100%), `level_line`. commands/escapes.rs —
read-only команда. CONFIG_KEYS 35→36 (`AUTONOMY`, дефолт 1 — только
голосование отображением: `devh merge` допечатывает
`автономия L1 (streak 13, корпус 13/13): ревью выборочное — 2 из 3
можно пропустить`; механических запретов нет — решение человека,
цифры машинные). RunConfig.autonomy + init-шаблон.

**Корпус пишет suitescript, не агент:** после adversarial-раздела
test.sh кладёт `.harness/corpus.json` = {"caught":ADV_OK,"total":ADV_N,
"date":…} — доля пойманных атак. Без прогона → total 0 → L0: уровень
нельзя получить «из воздуха».

**Баш-секция 47 (14 проверок):** ножи SMOKE_ESCAPE/SMOKE_SESSIONS/
SMOKE_CORPUS в mk_smoke_repo.sh — файлы в .harness/ через `git add -f`
(.harness/ в .gitignore; сессия бежит в worktree = чистый checkout —
урок A7); 47a пустой ledger → L0 без данных; 47b погашенный побег +
streak 12 + корпус 13/13 → L1; 47c долг (`-`) → L0 при любом
streak/корпусе; 47d `devh run` НЕ мержит (happy-path: run → merge) —
merge печатает строку при AUTONOMY=1 и молчит при 0, streak растёт на
сессию из log.md; 47e мусор в ledger'е отсеян.

**Приёмка:** unit 179 + integration 56 зелёные (8 юнитов core/escapes.rs:
parse полный/debt/мусор, границы уровня, streak, группы классов,
corpus json+rate, формат level_line + 4 юнита `report()`: пустой
ledger/группы+L1/долг режет уровень/нет корпуса ≠ 100%); bash-сьюта
PASS=425 FAIL=0 (411 A8 + 14 B1); fmt/clippy чистые; мутационный
аудит зоны B1 = 130 мутантов in-place → **129 caught / 0 missed /
1 unviable** (kill-rate 99.2% testable, ~50с/мутант).

**Правило unviable (жёсткое, заказчика — детали fast_cycle §P9):**
unviable — это дыра в покрытии (строка без kill-truth), а не
«не считается». Лечим КОД, чтобы мутант компилировался, — не
объясняем инструментом. Кейс B1: `autonomy_level ->
Default::default()` не собирался, у `Level` не было `Default` →
дерив + `#[default] L0`, повторный прогон с `--iterate` → Caught.
Экраны (`exclude_re`/`skip_calls`) = прокрастинация, допустимы
только с записанной причиной.

## B1 — Ledger побегов + уровень автономии

- **`.harness/escapes.md`**, строка: `2026-10-19 | W-042 | найдено: ручной дифф после merge | класс: blind:boundary | реакция: стаб-кандидат №14`. Правило (твоё, «обход только записанный»): каждый побег обязан конвертироваться — стаб / гейт / записанное «принял риск». Ledger без конвертации = debt.
- **`devh escapes`** — только чтение/агрегация (никаких `--add`; файл правится руками, как observations): группировка по классам, streak = сессий с log.md с последнего побега, вывод kill-rate корпуса из последнего прогона test.sh (test.sh дописывает `.harness/corpus.json`: `{"caught":13,"total":13,"date":…}`).
- **Чистые функции:** `parse_escape_line`, `autonomy_level(escapes: &[Escape], streak: u32, corpus_kill: f64) -> Level` — правило закреплено кодом и тестами: `L1 требует streak≥10 && corpus 100%`, `L2 — streak≥30`. Ключ `AUTONOMY` в конфиге — только** голосование отображением**: `devh merge` в конце печатает `автономия L1 (streak 12, corpus 13/13): ревью выборочное — 2 из 3 можно пропустить`. Механического запрета не делаем — для solo это театр; решение твоё, цифры — машинные.
- Мутации на `autonomy_level` — 0 missed (граничные streak=9/10, kill=0.99/1.0).

**Цена:** ~2 ч. **Смысл:** это единственное место, где «доверяю харнессу» перестаёт быть вайбом и становится числом.

---

# Серия C: контракт задачи (docs/devh_contract.md)

**Сквозной ход серии:** P8-5 (OBSERVE в спеке) **не имплементировать как отдельный `observe.md`** — поглотить в `contract.toml`. Один артефакт вместо трёх (spec-секции → observe.md → prompt-инжект). Это экономит сессию и убирает двойную правку.

## C1 — `contract.toml` в item

```toml
[task]
goal = "валидация нечисловых аргументов add"       # из первой строки спеки
[observe]                                          # из секции OBSERVE: (бывший P8-5)
points = ["return: add x 3 -> ERR, rc=1"]
[files]                                            # из секции FILES: → allowlist FREEZE (A6!)
touch = ["src/**"]
[red]
expect = "add_non_numeric"                          # из RED-EXPECT: (P7 — уже есть)
```

- `devh new` парсит секции спеки → пишет `items/W-NNN/contract.toml` (`commands/new.rs`, симметрично `red_from_spec`).
- **Чистая проверка** `contract_check(c: &Contract, diff_files: &[String]) -> Vec<String>`: goal непуст; `files.touch` покрывает дифф (или warning — FREEZE уже гейтит жёстко, контракт мягко предупреждает про НЕзаявленные правки наоборот: дифф меньше заявленного = «заявлено, не тронуто»); observe ≥ 1 точки; red.expect есть при наличии red.cmd.
- **`agent_prompt`** втыкает контракт целиком (это и есть «контракт переживает fix-раунды»: каждый раунд перечитывает goal+observe, а не историю).
- Corpus: good-стаб уже пишет OBSERVE-строку — секция S27 расширяется ассертом «в промпте агента (лог W-001-agent.log) есть goal и observe-точка».

**Цена:** ~3 ч.

**Приёмка C1 (2026-10-21):** аудит зоны коммита `991095c` (`cargo-mutants --in-diff`, nextest, `--baseline=skip`, in-place): **76/76 ✓ caught, 0 ✗ missed / 0 ⏱ timeout / 0 ⊘ unviable — kill-rate 100.0%** (contract.rs 43, tui.rs 29, prompt.rs 2, gates.rs 2; сумма фаз 37.4 мин, средняя фаза 14.7 с). Сьюты на main: unit 202 + integration 56 — зелёные, fmt чистый. Watcher заодо: singleton (flock `.harness/mutants-watch.lock` + sweep + pid-файл — «перезапустил, а старое живёт» невозможно), orphan-guard (PPID сменился → выходим, мёртвый pipe не копим), отчёт по проблемным МУТАНТАМ сразу после live-CLI. Гейт дрейфа `section00b` вернулся в main мержем `devh/b1-autonomy` (главная грязь сессии: C1-код жил в worktree-ветке, main его не видел — первая приёмка 125/125 шла по B1-диффу escapes-ledger и сертифицировала не ту зону; корректная зона C1 = дифф самого коммита C1).

## C2 — REVIEW против контракта + вторая модель

- **Ключ `REVIEW_AGENT`** (дефолт: наследует `AGENT` — обратная совместимость; в шаблоне init — комментарий с примером другой модели). `review_gate` использует его вместо `store.read(agent)`. Это самый дешёвый способ закрыть «self-review тем же агентом»: один ключ.
- `prompt::review_prompt` получает контракт: цель, инварианты, observe-точки; формат вердикта расширяется опциональной строкой `OBSERVE-COVERED: 2/2` (парсер `parse_review_verdict` + одна ветка — advisory, не вердикт).
- Corpus-кейсы 11/12 уже держат формат; добавляется `review-contract-miss.sh`: вердикт `ISSUES: observe-точка 1 не покрыта` → fix-раунд.

**Цена:** ~2 ч. Решение «ставить ли вторую модель» остаётся за B1-данными (если класс побегов не review — не ставить).

## C3 — `devh draft`: спек-фабрика

- `commands/draft.rs`: `devh draft "<intent>"` → `prompt::draft_prompt(repo_map, rules, hot_spots, intent)` → агент → черновик spec.md в `.harness/draft-N.md` (структура: Сделай/OBSERVE/FILES/RED/RED-EXPECT — те же секции, что парсит `devh new`).
- `repo_map` — статичная карта из D1 (AGENTS.md); `hot_spots` — killmap из P8-3, если он уже влит; без него драфт просто суше.
- Человек редактирует 2–5 минут → `devh new < draft-N.md` (твой существующий путь `devh new <N.md>`). Гейты не ослабляются ни на строку.

**Цена:** ~2–3 ч.

---

# Серия D: репо как контекст (docs/devh_context.md)

## D1 — AGENTS.md + rot-checker + холодный бенчмарк

- **`AGENTS.md`** в корне uwa (де-факто конвенция для LLM-читателей): карта модулей devharness в НОВОЙ структуре (драйвер run() 84 строки / фазы / core::gates судьи / state fold / shell-порты); список инвариантов (каждый — одна строка + файл); **таблица диалектов гейтов** — какие форматы парсят судьи (это чистое золото для агента: чтобы написать инструмент или стаб, он обязан знать диалект); «куда смотреть за X».
- **Rot-checker** `tests/context_rot.sh` (в test.sh, минуты): извлечь из AGENTS.md все пути `devharness/src/…/*.rs` → `test -f` каждый; упомянутые символы (`rg -c "fn <name>"`) ≥ 1. Протухший клейм — механически невозможен, твой принцип «клейм цитирует файл» в форме теста.
- **`QUESTIONS.md`** — 10 вопросов+эталонов («где живёт judge_red_pre?», «какой rc у env-блокера?», «какой формат вывода ждёт MUTATE-инструмент?»). Ритуал холодного бенчмарка: раз в 2 недели новому контексту (чистая сессия) дать ссылку на репо + вопросы; ≥9/10 — контекст жив. Автоматизировать LLM-вызов не буду — это твой ручной smoke мышления, 15 минут.

**Цена:** ~2–3 ч (в основном письмо AGENTS.md).

---

# K1 — Kani на ядро state (опционально, вишенка)

`#[cfg(kani)]`-харнессы на `transition`/`fold`/`claim_action`: доказать (а) fold никогда не паникует и терминален для любого &[Event], (б) claim_action тотальна и согласована с transition (`RejectDone ⟺ Done`), (в) инвариант replay: `fold(&events_of(s)) == s` для достижимых s. Три чистые функции — идеальные Kani-цели. CI: отдельный job с ручным/workflow_dispatch-триггером (kani тянет nightly — тяжело для каждого пуша). **Цена:** ~4 ч. **Честная оговорка:** при 96%-м юнит-покрытии этих функций — это престиж и дисциплина, не bug-catch; делать последним.

---

# Порядок, зависимости, цены

| # | Сессия | Зависит от | Цена | Метрика, которую приносит |
|---|---|---|---|---|
| A4 | smoke-скелет + S27 | — (A1–A3 ✓) | 3–4 ч | «работает в целом», <60 с |
| A5a | корпус: читеры агента | A4 | 3 ч | kill-rate 6/6 |
| A5b | корпус: читеры гейтов | A5a | 3 ч | kill-rate 12/12 |
| A6 | FREEZE-гейт | A4 (RED=стаб) | 3 ч | 13/13; дыра №1 закрыта |
| A7 | chaos-e2e | A4 (+A3 ✓) | 3–4 ч | инварианты под kill -9 |
| A8 | flaky v1 | — | 2 ч | flaky-стрк не молчит |
| B1 | escapes + автономия | A5b (corpus.json) | 2 ч | streak/уровень числом |
| C1 | contract.toml | A6 (files.touch) | 3 ч | OBSERVE живёт в одном артефакте |
| C2 | REVIEW_AGENT + контракт в ревью | C1 | 2 ч | вторая голова — опцией |
| C3 | devh draft | C1 (+P8-3 желательно) | 2–3 ч | фикс-раунды ↓ (меряется в log.md) |
| D1 | AGENTS.md + rot-checker | — | 2–3 ч | контекст не гниёт |
| K1 | Kani | — | 4 ч | теоремы вместо тестов |
| E1 | falsification уровня: сэмпл-ревью пропущенных → escape `blind:level` | B1 | 1 ч | 10/30 из взятых чисел → калибруемые |
| E2 | `is_debt` строгий дефолт (только `гейт:`/`стаб:`/`принял риск:`) | B1 | 0.5 ч | ложно-пониженного уровня ↓ |
| E3 | свежесть корпуса: CORPUS_MAX_AGE_DAYS, иначе «нет данных» | B1 | 0.5–1 ч | PERFECT ≠ «вчера был» |
| E4 | streak: Option-даты + WARN при >50% непарсящихся сессиях | B1 | 0.5 ч | молчаливого зануления нет |
| E5 | REVIEW: вторая модель по B1-данным (само для себя не ревьюит) | C2 | 2 ч | shortfall ↓ (B1 измерит остаток) |
| E6 | chaos: I/O-отказы (ENOSPC/частичная запись/git-ошибка) | A7 | 1.5–2 ч | дым → доказательство |

~11 сессий ≈ 32–36 ч чистого времени + серия E ниже (дёшево, ≈ 5–7 ч, три пункта — по одной строке, делаются между сессиями). Порядок жёсткий только внутри A (4→5→6; 7 после 4); B/C/D параллелеть как угодно. Каждая сессия — через собственный харнесс с RED-first; корпус A5 после A6 частично перепишет свои ассерты (ожидаёмо — это не «правка тестов», это добавление 13-го кейса; в spy-таблице `rust_build.md` зафиксируй).

---

# Серия E: слабые звенья — что НЕ выдержит фальсификацию (пост-B1 аудит, 2026-10-20)

**Детальное решение по каждому пункту (механика на уровне кода, RED-спеки,
границы, грабли) — `docs/E_series.md`.** Ниже — сжатые решения; расхождения
нет, E_series.md — источник истины. **Порядок запуска: сразу после текущей
сессии (C1) — серия E дёшевая и калибрует источники чисел, которыми
пользуются C2/C3/D1.**

Список по убыванию значимости. Уровень автономии и streak — числа, но числа
взяты с слепых зон: каждый пункт ниже дёшево лечится, и после лечения число
становится калибруемым, а не просто большим.

## E1 —**Решение (E_series.md):** сэмпл Детерминированный — `W-id % AUTONOMY_SAMPLE == 0`
(`sampled_ids(log_md, every)`; харнесс выдаёт номера, агент не влияет).
`devh escapes --sample` печатает выбранные W-id + ГОТОВУЮ команду (`git show <sha>`
из log.md) — friction убивает петлю. Чистая выборка тоже записывается:
`.harness/samples.md` (`дата | W-NNN | чисто` / `| escape: blind:level`), report
показывает `выборка: 2/2 чисто; blind:level ×1`. В v1 НЕ заводить в
`autonomy_level` — копим калибровочные данные (как B1: инструмент измеряет,
решение за человеком). Doctor: «выборка отстаёт» — единственная защита от
добровольности петли.
 Autonomy не имеет петли фальсификации

L1 = «2 из 3 можно пропустить» — но никто не проверяет, ловит ли оставшаяся
1/3 ревью побеги. Уровень — функция от *замеченных* побегов и *известного*
корпуса; слепота внимания не измеряется by construction.

**Лечение:** даже на L2 выборочно ревьюить пропущенные сессии (сэмпл 1 из N,
ключ `AUTONOMY_SAMPLE`) и расхождение записывать как escape класса
`blind:level` — он идёт в тот же ledger, значит калибрует уровень. 10/30 из
взятых чисел станут калибруемыми. **Цена:** ~1 ч.

## E2 —**Решение (E_series.md):** инверсия дефолта грамматикой префиксов —
`const CONVERTS: [&str; 3] = ["гейт:", "стаб:", "принял риск:"]`;
`is_debt = !CONVERTS.iter().any(|p| r.starts_with(p))` (reaction trim+lowercase).
RED-спека: «ok/later/wontfix/починил руками/стаб-кандидат №14» = долг,
«гейт:…/стаб:…/принял риск:…/ГЕЙТ: x» = конвертация. Грабля: фикстура B1
`"стаб-кандидат №14"` переписывается в `"стаб: кандидат №14"` — смена контракта,
не «правка теста»; миграция живого `.harness/escapes.md` руками до слияния;
`report()` обязан печатать грамматику, иначе человек пишет «ok» и не понимает L0.
 `is_debt` мягкий по умолчанию

Реакции «ok», «later», «wontfix» считаются конвертацией тикета — долг закрыт
навсегда. Цена ложно-пониженного уровня — лишнее ревью; цена
ложно-повышенного — пропущенный мусор.

**Лечение:** строгий дефолт — конвертация ТОЛЬКО по явной грамматике
(`гейт:`, `стаб:`, `принял риск:`), остальное остаётся долгом. Одна строка в
escapes/ledger. **Цена:** 30 мин.

## E3 —**Решение (E_series.md):** референс возраста — дата ПОСЛЕДНЕЙ СЕССИИ в log.md
(не wall clock — тестируемо; лог уже единые часы). Без chronо: Hinnant
`days_from_civil` (~10 строк), `days_between(a,b) -> Option<i64>`,
`staleness(corpus_date, ref, max) -> Fresh | Stale{age} | Unknown`. Stale/Unknown →
уровень не повышается, но отчёт показывает РЕАЛЬНЫЕ цифры с пометкой
(`корпус: 13/13 (2026-10-01, устарел 21д > 14)`), не нули. Граница: `age == max`
→ Fresh (тест на границу — иначе мутант `>`↔`>=` живёт). corpus свежее сессии
(age ≤ 0) → Fresh, не Unknown. CONFIG_KEYS 36→37.
 Корпус не стареет

`13/13` от любой давности даёт PERFECT, streak тем временем растёт ежедневно.
Данные корпуса и streak живут в разном времени.

**Лечение:** порог свежести (N дней, иначе «нет данных» — уровень не
повышается): ключ `CORPUS_MAX_AGE_DAYS`, дата из `corpus.json`. **Цена:**
30–60 мин.

## E4 —**Решение (E_series.md):** типизированная статистика, а не голый WARN —
`LogHealth{headers, dated}`; `streak_of(...) -> (u32, distrust)`,
`distrust = headers > 0 && undated*2 > headers` (>50%;RED: 6/10 → true, 5/10 →
false; пустой лог → false — «нет данных» ≠ «ложь»). Светит в `report()` строкой и
doctor soft-warn; `level_line` не менять. Опция B (+30 мин, вторым коммитом):
у побега есть `session: "W-095"` — считать заголовки ПОСЛЕ него в файле
(append-only log.md = монотонные часы); дата — фолбэк.
 Streak — лексикографическое сравнение дат с точностью до дня

Сессии того же дня, что побег, считаются «до»; дрейф формата заголовков
`log.md` зануляет streak молча.

**Лечение:** парсер дат возвращает `Option`; если больше половины сессий не
парсятся — WARN в doctor/merge: «лог нечитаем, streak недоверенный».
**Цена:** 30 мин.

## E5 — REVIEW — самое мягкое звено цепи

Self-review остаётся вторым запросом тому же агенту. B1 честно измерит
остаток (ожидаю класс `blind:review` в ledger), но важно понимать: B1
измеряет остаток, а не сокращает его. Лечение — C2 (REVIEW_AGENT + контракт
в ревью): решение «ставить ли вторую модель» остаётся за B1-данными.
**Цена:** входит в C2 (2 ч).

## E6 —**Решение (E_series.md):** не ENOSPC через tmpfs (CI непереносимо — нет root) —
моделировать I/O-сбой его СЛЕДСТВИЕМ на диске: (1) state-corruption — сид
портит status/events.log/corpus.json/escapes.md/сирота `.tmp` (обрыв строки,
мусор, пусто) → recovery + check_invariants (инварианты A7 уже написаны — бить
направленно); (2) remote-failure — недоступный URL / `chmod -w` bare-репо →
run/merge падают ЧИСТО (rc≠0, lock снят, worktree цел, doctor жив);
(3) quota — `ulimit -f 1` → SIGXFSZ; платформа не даёт → SKIP с причиной, не FAIL.
Приёмка: 3 класса × 5 сидов; `devharness-chaos.yml` matrix `{kill, io}`; секция
45b. Оговорка для rust_build.md: моделируется СЛЕДСТВИЕ сбоя, инъекция «в момент
записи» — только с портами эффектов (v2).
 A7 — только SIGKILL; I/O-уровень отказов не инъектируется

ENOSPC, частичная запись, git-ошибка — вне `kill -9`-сценариев. 5×2 сида —
дым, не доказательство; ночная канарейка это компенсирует со временем.

**Лечение:** фаза 2 A7: инъекция I/O-сбоев (tmpfs-квота на ENOSPC, частичная
запись в файловом стабе, ошибка git через стаб-порт), сид кратным 10.
**Цена:** 1.5–2 ч.

**Порядок:** E2 → E4 → E3 → E1 → E6; E5 = C2. Первые три — по одной строке
каждая, идут между основными сессиями; ни один пункт не двигает вердикты
гейтов — они калибруют ИСТОЧНИК чисел (ledger/streak/corpus), а не сами
пороги.

| Сессия | Контент | Секции test.sh | Мутационный аудит |
|---|---|---|---|
| E-1 | E2 + E4 | 47 (upd) | escapes.rs in-diff |
| E-2 | E3 | 47b | escapes.rs + civil-days |
| E-3 | E1 | 48 | escapes + commands |
| E-4 | E6 | 45b | chaos.sh — shellcheck, .rs не тронуты |
| C2 | E5 (+ REVIEW_BLIND) | 49 | gates/review |
| D-1 | D1 | doctor-расширение | doctor.rs |
| K1 | Kani | — (отдельный workflow) | kani-harness'ы |

Уточнение оценки: «одна строка» в E2/E4/E3 — это строка кода ПЛЮС фикстуры,
тесты и rust_build.md; закладывай 40–60 мин на пункт.

## E-1 (E2 + E4) — СДАН (2026-10-21, main, сессия E-1)

**E2 — грамматика конвертации (`core/escapes.rs`):** `is_debt` = инверсия
дефолта — долг ВСЁ, кроме явной грамматики `гейт:`/`стаб:`/`принял риск:`
(регистр не важен). «ок», «later», «стаб-кандидат №14» (без двоеточия),
«гейт NO-IGNORED» (без двоеточия) = долг → L0. Смена контракта B1
предсказана граблей №1 и произошла ровно: фикстуры `"стаб"`/`"гейт X"`
в юнитах, интеграции s47b и `test.sh` мигрированы в грамматику (`стаб: …`,
`гейт: X`) — это смена контракта, не правка теста. Живого
`.harness/escapes.md` в репе нет — миграция не потребовалась.
`report()` раскрывает грамматику в подсказке пустого ledger'а и в разделе
долга («реакция без префикса гейт:/стаб:/принял риск:») — грабли №3.

**E4 — здоровье журнала (`core/escapes.rs`):** `LogHealth{headers, dated}` +
`log_health(log_md)` (та же механика отбора заголовков, что `sessions_in`),
`undated()`, `distrust()` = без даты СТРОГО больше половины (граница 50%
зафиксирована тестами: 5/10 → достоверно, 6/11 → недоверенный; пустой лог
→ false, «нет данных» ≠ «ложь»). `streak_of` возвращает `(streak, distrust)`;
`aggregate` — 4-элементный кортеж. Уровень при distrust НЕ понижается
(мера остаётся консервативной) — предупреждение печатают `devh escapes`
(`⚠ журнал: N заголовков log.md без даты — streak занижен`), `devh merge`
(строка перед уровнем) и `devh doctor` (soft-warn, симметрия с flaky).

**Приёмка E-1:** unit 208 + integration 56 зелёные; `bash devharness/test.sh`
**437 PASS / 0 FAIL** (47f: вольная реакция «ок» = долг → L0 при streak 12;
`ГЕЙТ:` (регистр) = конвертация → L1; подсказка раскрывает грамматику;
`s47g` — журнал без дат даёт предупреждение при streak 0); fmt `-p devh`
чистый; clippy — находки только в предсуществующих тестах run.rs
(песочница с forbid-линтами, гейт здесь не по clippy). Два моих ожидания в
новых тестах были неверны (заголовок без даты — заголовок; 1/2 без даты —
ровно половина, доверие целое) — исправлены ожидания, не код.

**Дальше по нарезке:** E-2 (E3 — свежесть корпуса, Hinnant
`days_from_civil`, `CORPUS_FRESH_DAYS=14`) → E-3 (E1 — петля фальсификации,
`AUTONOMY_SAMPLE=3`) → E-4 (E6 — chaos фаза 2, state-corruption) → C2 = E5
(сначала `REVIEW_BLIND=1` за 15 мин) → D1 → K1.

**Чего серия E осознанно не делает:**
1. **E1 остаётся добровольным** — сэмпл-ревью делает человек, гейт не заставит; максимум — готовая команда + напоминание doctor. Петля фальсификации не бывает принудительной.
2. **«2 из 3» — эвристика навсегда.** Калибровка делает её честной (известен base rate пропусков на каждом уровне), не доказательством.
3. **Корпус A5 стареет вместе с харнессом** — E3 меряет возраст записи corpus.json, не актуальность атак к новым гейтам; «атака без соответствующего гейта в текущем коде = кандидат на переработку» — тем же механизмом позже.


---

# Что ещё осталось (явный список)

**Не раскрыто в этом сообщении до конца — напишу следующей серией по запросу:**
1. Полные листинги оставшихся стабов A5 (red-green, red-wrong, ignored-test, mutate-killer, freeze-cheat, slow-mutate — у меня есть каждый, просто не влезает);
2. Полный текст `AGENTS.md` (D1) — черновик на ~120 строк, включая таблицу диалектов гейтов;
3. Текст `draft_prompt` (C3) и `QUESTIONS.md` (D1);
4. Kani-харнессы (K1) — код `#[cfg(kani)]`-блоков;
5. Схема nightly-workflow `devharness-canary.yml` (A8).

**Стратегические остатки (вне этого плана, но в очереди):**
- **P8-1..P8-4 (third loop: survivors → observations, killmap, hot-spot, пенсия)** — план написан, не имплементирован; C1 поглощает P8-5, остальное остаётся самостоятельным (killmap особенно — он кормит C3 и hot-spot'ы);
- **решение о второй модели в REVIEW** — отложить до B1-данных о классах побегов;
- **ретировка devh-classic** — твои критерии сгорают 2026-10-30 (14 дней от edd64c6): проверить оба CI зелёные/ноль патчей/все сессии через обёртку — и либо `git rm devh-classic selfmutate.sh`, либо продлить;
- **REVIEW-гейт как таковой** — даже с контрактом (C2) это остаётся самым мягким звеном; долгосрочная мысль — внешний ревьюер-стаб в корпусе с «verdict-моделью», но это после B1.

---

Продолжаю оборванную мысль — третий пункт: e2e/smoke и совокупность.

**Почему e2e — слабое доказательство.** Юнит-тесты + мутации доказывают узлы графа: «каждая функция видима своими тестами». Совокупность — это рёбра (контракты между модулями) и траектории (последовательности состояний во времени). Обычный e2e-сценарий — один прогон одной траектории с одним булевым исходом на гигантском пространстве путей. Разрешение слишком низкое: он ловит «всё сломалось совсем», но не «контракт разъехался на 2%».

**ae9f619 — это ровно баг совокупности.** Каждый компонент по отдельности был прав: cargo-mutants умеет восстанавливать, git add добавляет, git status показывает. Сломалась траектория — мутация пережила окно между restore и add, а `head -3` срезал третьего участника. Ни один юнит-тест этот класс не видит by construction.

**Единственный честный e2e — со стаб-агентом.** Агент — единственный недетерминированный элемент в системе. Замени его записанным транскриптом (replay), и вся остальная композиция становится детерминированной — а значит, прогонимой многократно, сажаемой под мутации и сжимаемой. Это тот же приём, что golden fixtures, но на уровне всей системы: fossils на швах — replay парсеров, fossil верхнего уровня — replay агента.

**Лестница доказательств совокупности:**

1. **Швы → контракты.** Перечислить все швы: devh→core, core→cargo-mutants, core→git, core→агент, core→fs/lock, watchdog→run. На каждом — тип (Verdict, GateOutcome), golden fixture реальных транскриптов, proptest на парсере: round-trip, malformed, version drift. Индукция по графу: узлы закрыты мутациями, рёбра — свойствами контрактов.

2. **Машина состояний самого харнесса.** Stateful proptest: Idle→Locked→Baseline→Mutate(k)→Test(k)→Verdict(k)→Restore→Unlocked. Генерируешь последовательности команд + отказы, инварианты проверяются после каждого шага, failing-кейс сжимается до минимального расписания. Баг ae9f619 ловится генерацией «SIGKILL между mutate и restore» — сжатие привело бы ровно к нему.

3. **Run-as-test.** Инварианты на каждом реальном прогоне: worktree при выходе байт-в-байт равен входному (если вердикт не говорит иного); lock держится ⇔ процесс жив; ровно один вердикт за прогон; временных файлов не осталось; kill-count монотонен; RED в логе раньше первого коммита кода. Это превращает dogfooding из анекдота в N прогонов × инварианты — непрерывный e2e бесплатно.

4. **Fault injection по портам.** Порты эффектов (Process/FS/Git/Clock) в v2-чертеже — не эстетика, а точки инъекции: ENOSPC, EIO, SIGKILL в выбранной точке, git печатает неожидаемое, cargo-mutants сменил формат. Баги совокупности живут в проводке ошибочных путей — порты делают их инъекционными. В bash-версии швы — строки в пайпах: непроверяемо by construction.

5. **Burn-in + телеметрия.** Остаток — timing и матрица сред (flock-семантика, версии git). Это не доказывается, это переживается — но теперь burn-in идёт с инвариантами из п.3, а не вслепую.

| Класс бага | Пример | Ловит |
|---|---|---|
| Контракт дрейфнул | cargo-mutants 28 меняет JSON | golden + version-drift proptest |
| Ошибочная проводка | restore теряется при Err | fault injection по портам |
| Траектория/прерывание | ae9f619 | stateful машина состояний |
| Поведение агента | нестабильный RED | стаб-replay транскриптов |
| Матрица сред | flock  | burn-in с инвариантами |
| Тайминг/гонки | статичный watchdog | loom (если появятся потоки), пока телеметрия |

**Мета-правило.** Каждый новый оракул сам садится под мутационный гейт: проверяльщики инвариантов мутятся как обычный код, иначе оракул слеп. Это точный аналог rustc — диагностики компилятора проверяются ui-тестами с ожидаемым выводом, теми же золотниками.

**Что честно остаётся за кадром.** Настоящая конкурентность (пока нет потоков — нечего доказывать), полная матрица окружений, поведение агента как таковое. Полного доказательства совокупности не существует ни за какие деньги — это потолок тестирования вообще. Но лестница даёт композиционное покрытие вместо сценарного: булево на каждую (траектория × отказ), со сжатием и локализацией, вместо одного булева на весь путь.

**Порядок внедрения по цене/эффекту:** сначала п.3 — инварианты на реальных прогонах почти бесплатны, и прецедент уже есть (a19f1a6 — по сути первый инвариант, написанный постфактум). Потом п.1 на самом хрупком шве — output cargo-mutants. Потом машина состояний. Сценарные e2e оставить как smoke, не как доказательство.

Если ложить это в документацию — это логичный раздел рядом с devh_third_loop.md, потому что третья петля закрывает слепоту тестов, а эта — слепоту сборки: симметричная проблема, симметричное решение.

