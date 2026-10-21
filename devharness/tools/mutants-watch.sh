#!/usr/bin/env bash
# tools/mutants-watch.sh — event-based watcher прогона cargo-mutants.
#
# Зачем: аудит зоны/ядра идёт десятки минут; `sleep`-петли в сессии слепы
# (не видно, сколько осталось) и дороги (каждый poll = отдельный turn).
# Скрипт ждёт СОБЫТИЕ (inotify: outcomes.json перезаписан / процесс
# завершился), а не по таймеру, и рисует ОДНУ строку прогресса:
# процент готовности, счётчики исходов, среднее на мутант, ETA, текущий
# мутант. Над строкой — легенда значков (что есть что).
#
# Счётчики: ✓ caught (пойман) · ✗ missed (выжил — дыра в тестах) ·
#           ⏱ timeout (повес) · ⊘ unviable (не компилится — дыра в покрытии)
#
# Использование:
#   tools/mutants-watch.sh                 # ждать финиша прогона c прогрессом
#   tools/mutants-watch.sh --once          # один снимок и выйти
#   tools/mutants-watch.sh --interval 3    # тик-страховка, сек (по умолчанию 5)
#   MUTANTS_LOG=/tmp/mut.log tools/…       # откуда читать "Found N mutants to test"
#   NO_COLOR=1                             # без ANSI; в не-tty — троттлинг вывода
#
# Выход: 0 — прогон завершён (или снят снимок); 2 — не найден mutants.out.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$HERE/../.."

# --- singleton: живой CLI ровно один, копить watcher'ы невозможно ----------
# Грабля, из которой вырос блок: каждый «дай live cli» поднимал новый watcher,
# старые продолжали жить и печатать в мёртвые пайпы — оператор видел чушь или
# пустоту, а 'pkill -f mutants-watch' настиал self-match. Решение: атомарный
# файловый лок + список pid'ов. Лок держит ровно один процесс; чужой держатель
# (его pid в pid-файле) гасится по TERM — побеждает ПОСЛЕДНИЙ запуск; после
# захвата лока подметаем всех прочих живых watcher'ов, кроме себя и предков
# (предки = launcher/env цепочка текущего запуска, их трогать нельзя).
RUNDIR="$HERE/../../.harness"       # корневой .harness репы (gitignored)
LOCK="$RUNDIR/mutants-watch.lock"
PIDF="$RUNDIR/mutants-watch.pid"
mkdir -p "$RUNDIR"

mykin=" $$ "                        # себя + цепочку предков (не трогаем)
p="$$"
while :; do
    pp="$(awk '/^PPid:/{print $2}' "/proc/$p/status" 2>/dev/null || true)"
    [ -n "$pp" ] || break
    if [ "$pp" = "0" ] || [ "$pp" = "1" ] || [ "$pp" = "$p" ]; then break; fi
    mykin="$mykin$pp "
    p="$pp"
done

sweep_watchers() {                  # зачистка всех watcher'ов вне нашего рода
    local q
    for q in $(pgrep -f 'tools/mutants-watch\.sh' 2>/dev/null || true); do
        case "$mykin" in *" $q "*) continue ;; esac
        kill "$q" 2>/dev/null || true
    done
}

exec 9>"$LOCK"
if ! flock -n 9; then
    # лок занят живым watcher'ом — гасим его (последний запуск выигрывает)
    other="$(cat "$PIDF" 2>/dev/null || true)"
    if [ -n "$other" ] && [ -r "/proc/$other/cmdline" ] \
        && tr '\0' ' ' <"/proc/$other/cmdline" | grep -q 'mutants-watch\.sh'; then
        kill "$other" 2>/dev/null || true
    fi
    for _ in $(seq 1 10); do        # ждём освобождения лока до 2с
        flock -n 9 && break
        sweep_watchers              # страховка: держатель без pid-файла
        sleep 0.2
    done
fi
echo "$$" >"$PIDF"
sweep_watchers                      # и левые экземпляры (ручной запуск и пр.)

trap 'if [ "$(cat "$PIDF" 2>/dev/null)" = "$$" ]; then rm -f "$PIDF"; fi; report' EXIT
trap 'exit 0' TERM INT              # дать отработать EXIT-хуку

OUT="${MUTANTS_OUT:-mutants.out}"
OUTCOMES="$OUT/outcomes.json"
MUTANTS_JSON="$OUT/mutants.json"
LOG="${MUTANTS_LOG:-$OUT/debug.log}"
DIFFDIR="$OUT/diff"
INTERVAL="${INTERVAL:-5}"
STEP="${STEP:-5}"      # в не-tty печатаем каждые STEP готовых мутантов
ONCE=0
LAST_PRINTED=0
LEGEND_DONE=0

while [ $# -gt 0 ]; do
    case "$1" in
        --once)     ONCE=1; shift ;;
        --interval) INTERVAL="$2"; shift 2 ;;
        --step)     STEP="$2"; shift 2 ;;
        -h|--help)  awk '/^# ---usage---/{f=1;next} /^# ---cut---/{f=0} f' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *)          echo "unknown arg: $1" >&2; exit 64 ;;
    esac
done

# Гонка старта: cargo-mutants создаёт mutants.out не мгновенно (первые секунды
# уходят на сканирование дерева). Ждём до 90с, иначе мутатор правда не запущен.
for _ in $(seq 1 90); do
    [ -d "$OUT" ] && break
    sleep 1
done
[ -d "$OUT" ] || { echo "нет $OUT — аудит не запускался?" >&2; exit 2; }

COLOR=0
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then COLOR=1; fi
if [ "${HARNESS_COLOR:-}" = "always" ]; then COLOR=1; fi
export COLOR

# ---usage---
#   tools/mutants-watch.sh                 # ждать финиша прогона c прогрессом
#   tools/mutants-watch.sh --once          # один снимок и выйти
#   tools/mutants-watch.sh --interval 3    # тик-страховка, сек (по умолчанию 5)
#   MUTANTS_LOG=/tmp/mut.log tools/…       # откуда читать "Found N mutants to test"
#   NO_COLOR=1                             # без ANSI; в не-tty — троттлинг вывода
#
# ЖИВОЙ CLI (одна строка обновляется на месте, \r\033[K ПЕРЕД строкой) —
# канон для агента: БЕЗ лог-файла и БЕЗ setsid/nohup. stdout watcher'а
# идёт НАПРЯМУЮ в вывод tool'а — оператор видит строку прямо там. Любая
# запись кадров в файл (/tmp/live.log и т.п.) ЗАПРЕЩЕНА: это дубль статуса
# аудита, оператор видит только пустой лог и кричит «ничего не вижу».
# Запуск из корня репы, аудит уже идёт:
#   env LIVE=1 HARNESS_COLOR=always STEP=1 INTERVAL=2 \
#     MUTANTS_LOG=/tmp/c1c_audit.log \
#     devharness/tools/mutants-watch.sh
#
# Singleton: живой CLI всегда ОДИН процесс. Новый запуск сам гасит старые
# watcher'ы (лок .harness/mutants-watch.lock + подметание по pgrep, кроме
# себя и своих launcher'ов). Ручной pkill/kill перед перезапуском не нужен.
# ---cut---

# легенда — ОДИН раз, строкой выше прогресса (потом только строка прогресса)
legend() {
    if [ "$LEGEND_DONE" = "1" ]; then return 0; fi
    LEGEND_DONE=1
    python3 - <<'PY'
import os
color = os.environ.get("COLOR") == "1"
C = {"ok": "\033[32m", "bad": "\033[1;31m", "warn": "\033[33m",
     "mag": "\033[35m", "dim": "\033[2m", "off": "\033[0m"} if color else \
    {k: "" for k in ("ok", "bad", "warn", "mag", "dim", "off")}
pairs = [("\u2713", "ok", "caught"), ("\u2717", "bad", "missed"),
         ("\u23f1", "warn", "timeout"), ("\u2298", "mag", "unviable")]
parts = ["%s%s%s %s" % (C[c], s, C["off"], name) for s, c, name in pairs]
print("%sлегенда:%s %s" % (C["dim"], C["off"], "  ".join(parts)))
PY
}

# текущий мутант — самый свежий .diff (появился во время билда/теста)
current() {
    local f n
    f="$(find "$DIFFDIR" -maxdepth 1 -name '*.diff' -printf '%T@ %p\n' 2>/dev/null \
        | sort -rn | head -1 | cut -d' ' -f2- || true)"
    [ -n "$f" ] || return 0
    n="$(basename -- "${f##*/}" .diff)"
    n="${n%%_col_*}"
    n="${n//__/_}"                         # cargo-mutants 27: кодирует / как __
    n="${n#*_src_}"                        # devharness_src_ → от корня крейта
    n="$(printf '%s' "$n" | sed -E 's/_line_([0-9]+)$/:\1/')"
    n="${n//_//}"                          # вложенные каталоги через _
    printf '%s' "$n"
}

render() {
    local snap dn total counts avg_s last_s cur
    snap="$(python3 -u - "$OUTCOMES" "$MUTANTS_JSON" "$LOG" <<'PY'
import json, re, sys, collections
outcomes_p, mutants_p, log_p = sys.argv[1:4]
dn = 0; counts = collections.Counter(); avg = 0.0; last = ""
try:
    outs = json.load(open(outcomes_p)).get("outcomes", [])
    dn = len(outs)
    for o in outs:
        counts[o["summary"]] += 1
    durs = [p["duration"] for o in outs for p in o.get("phase_results", [])]
    if durs:
        avg = sum(durs) / len(durs)
    for o in outs:
        if o["summary"] in ("MissedMutant", "Unviable"):
            m = o["scenario"]["Mutant"]
            last = "%s %s:%s" % (o["summary"].replace("Mutant", ""),
                                 m["file"], m["span"]["start"]["line"])
except Exception:
    dn = 0
total = 0
try:
    total = len(json.load(open(mutants_p)))
except Exception:
    pass
try:
    for line in open(log_p, errors="replace"):
        m = re.search(r"Found (\d+) mutants to test", line)
        if m:
            total = int(m.group(1))
except Exception:
    pass
print("%d\t%d\t%s\t%.3f\t%s" % (dn, total, dict(counts), avg, last))
PY
)"
    IFS=$'\t' read -r dn total counts avg_s last_s <<<"$snap"
    cur="$(current)"
    legend
    python3 -u - "$dn" "$total" "$counts" "$avg_s" "$last_s" "$cur" <<'PY'
import os, sys, time
dn, total, counts_s, avg_s, last, cur = sys.argv[1:7]
dn = int(dn); total = int(total); avg = float(avg_s)
counts = eval(counts_s) if counts_s else {}
color = os.environ.get("COLOR") == "1"
C = {"ok": "\033[32m", "bad": "\033[1;31m", "warn": "\033[33m",
     "mag": "\033[35m", "info": "\033[36m", "dim": "\033[2m",
     "off": "\033[0m", "pbg": "\033[44m", "pfg": "\033[1;97m"} if color else \
    {k: "" for k in ("ok", "bad", "warn", "mag", "info", "dim", "off",
                     "pbg", "pfg")}

# --- процент готовности: цветная пилюля, вместо прогресс-бара ---
if total:
    p = min(100, dn * 100 // total)
    pct = "%s%s %3d%% %s" % (C["pbg"], C["pfg"], p, C["off"])
else:
    pct = C["dim"] + "  —%  " + C["off"]

# --- счётчики исходов (unviable всегда, по правилу fast_cycle §P9) ---
cnt = "%s\u2713%s %d  " % (C["ok"], C["off"], counts.get("CaughtMutant", 0))
for k, sym_c, col in (("MissedMutant", "\u2717", "bad"),
                      ("TimeoutMutant", "\u23f1", "warn"),
                      ("Unviable", "\u2298", "mag")):
    cnt += "%s%s%s %d  " % (C[col], sym_c, C["off"], counts.get(k, 0))
cnt = cnt.rstrip()

# --- ETA в минутах ---
if total and dn >= total:
    eta = C["ok"] + "готово" + C["off"]
elif avg > 0:
    eta = C["info"] + "%.1f мин" % (avg * (total - dn) / 60.0) + C["off"]
else:
    eta = C["dim"] + "\u2014" + C["off"]

ts = C["dim"] + time.strftime("%H:%M:%S") + C["off"]
tail = ""
if last:
    tail += "  " + C["mag"] + last + C["off"]
if cur:
    tail += "  " + C["warn"] + "\u25b8 " + cur + C["off"]
if os.environ.get("LIVE") == "1":
    # ЖИВОЙ кадр: стираем строку ПЕРЕД записью. Грабля (было): end="\r\033[K"
    # после строки — кадр стирал сам себя, на экране оставалась одна легенда.
    lead, end = "\r\033[K", ""
else:
    lead, end = "", "\n"
sys.stdout.write("%s%s %s %s  avg %.1f мин  ETA %s%s%s"
                 % (lead, ts, pct, cnt, avg / 60.0, eta, tail, end))
PY
}

# в tty — каждая строка; в pipe/логе — каждые STEP готовых мутантов (иначе шум).
# ЖИВОЙ режим (LIVE=1): кадр КАЖДЫЙ тик — при INTERVAL=1 раз в секунду.
# Метка времени в кадне меняется каждую секунду, поэтому оператору видно,
# что watcher живой и не завис, даже между мутантами (один мутант ~20-30с).
render_throttled() {
    local dn
    dn="$(cut -f1 <<<"$(python3 -u -c 'import json,sys; print(len(json.load(open(sys.argv[1])).get("outcomes",[])))' "$OUTCOMES" 2>/dev/null || echo 0)")"
    if [ "$LAST_PRINTED" = "-1" ] || [ "$(( dn - LAST_PRINTED ))" -ge "$STEP" ] || [ "${LIVE:-}" = "1" ]; then
        LAST_PRINTED="$dn"
        render
    fi
}

# ------------------------------------------------------------------ отчёт
# После live-cli — СРАЗУ отчёт «по которому идёт дальшая работа»: список
# проблемных МУТАНТОВ (✗ MissedMutant — выжил, дыра в тестах; ⏱ TimeoutMutant
# — мутант повис, его тест-фаза не уложилась в таймаут; ⊘ Unviable — мутант
# не компилится). ⏱/⊘ относятся ИМЕННО к мутантам, не к тулу и не к watcher'у.
# Итоговые строки — вход следующих действий: чинить тесты/тайминги/покрытие.
report() {
    [ "${REPORT_DONE:-0}" = "1" ] && return 0
    REPORT_DONE=1
    python3 -u - "$OUTCOMES" <<'PY' || true
import json, sys
try:
    outs = json.load(open(sys.argv[1])).get("outcomes", [])
except Exception:
    sys.exit(0)
if not outs:
    sys.exit(0)
SYM = {"MissedMutant": "\u2717", "TimeoutMutant": "\u23f1", "Unviable": "\u2298"}
LABEL = {"MissedMutant": "выжил — дыра в тестах",
         "TimeoutMutant": "мутант повис — таймаут тест-фазы мутанта",
         "Unviable": "не компилится — дыра в покрытии"}
bad = [o for o in outs if o["summary"] != "CaughtMutant"]
print()
print("\u2500\u2500 отчёт: проблемные мутанты (по ним дальняя работа) \u2500\u2500")
if not bad:
    print("   нет — все пойманы")
for o in bad:
    m = o["scenario"]["Mutant"]
    print("   %s %s:%s \u2014 %s [%s]" % (SYM.get(o["summary"], "?"),
          m["file"], m["span"]["start"]["line"], m["name"],
          LABEL.get(o["summary"], "")))
counts = {}
for o in outs:
    counts[o["summary"]] = counts.get(o["summary"], 0) + 1
tot = len(outs)
kr = 100.0 * (counts.get("CaughtMutant", 0) +
              counts.get("TimeoutMutant", 0)) / tot if tot else 0.0
print("   итог: %d мутантов \u2014 \u2713 %d  \u2717 %d  \u23f1 %d  \u2298 %d  | kill-rate %.1f%%"
      % (tot, counts.get("CaughtMutant", 0), counts.get("MissedMutant", 0),
         counts.get("TimeoutMutant", 0), counts.get("Unviable", 0), kr))
PY
}

# --- рецепт запуска ЖИВОГО CLI: ТОЛЬКО в --help и в этом комментарии. ---
# В КОНСОЛЬ НЕ ПЕЧАТАЕМ (оператор: «сразу вывод live cli, без простыни
# текста выше»): первый кадр = легенда + строка прогресса, ничего кроме.
#   env LIVE=1 HARNESS_COLOR=always STEP=1 INTERVAL=1 \
#     MUTANTS_LOG=/tmp/c1c_audit.log devharness/tools/mutants-watch.sh

# ------------------------------------------------------------------- цикл
if [ "$ONCE" = "1" ]; then
    if pgrep -x cargo-mutants >/dev/null; then echo "(прогон идёт)"; fi
    render
    report
    exit 0
fi
# ЖИВОЙ режим: одна строка, обновляемая \r на месте; первая — сразу.
LIVE=1
export LIVE
LAST_PRINTED=-1
LAUNCHER="$PPID"                # кто нас запустил (tool'шell/терминал)

# Сирота: launcher умер (tool' вернул таймаут, терминал закрыт) — печатать
# некуда, stdout висит в мёртвом pipe. Не копим мусор: выходим сами.
# (Ложных выходов нет: при штатном запуске PPID не меняется.)
while pgrep -x cargo-mutants >/dev/null; do
    [ "$PPID" = "$LAUNCHER" ] || exit 0
    render_throttled
    if command -v inotifywait >/dev/null 2>&1; then
        inotifywait -q -t "$INTERVAL" -e close_write,moved_to "$OUTCOMES" >/dev/null 2>&1 || true
    else
        sleep "$INTERVAL"
    fi
done

unset LIVE   # финальный кадр — с \n, чтобы остался на экране (не стёрся)
render
echo
