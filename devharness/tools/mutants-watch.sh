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

[ -d "$OUT" ] || { echo "нет $OUT — аудит не запускался?" >&2; exit 2; }

COLOR=0
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then COLOR=1; fi
export COLOR

# ---usage---
#   tools/mutants-watch.sh                 # ждать финиша прогона c прогрессом
#   tools/mutants-watch.sh --once          # один снимок и выйти
#   tools/mutants-watch.sh --interval 3    # тик-страховка, сек (по умолчанию 5)
#   MUTANTS_LOG=/tmp/mut.log tools/…       # откуда читать "Found N mutants to test"
#   NO_COLOR=1                             # без ANSI; в не-tty — троттлинг вывода
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
    n="${n#*_src_}"                        # devharness_src_ → путь от корня крейта
    n="$(printf '%s' "$n" | sed -E 's/_line_([0-9]+)$/:\1/')"
    n="${n/_rs:/.rs:}"                     # core_escapes_rs:249 → core_escapes.rs:249
    n="${n//_//}"                          # core_escapes → core/escapes
    printf '%s' "$n"
}

render() {
    local snap dn total counts avg_s last_s cur
    snap="$(python3 - "$OUTCOMES" "$MUTANTS_JSON" "$LOG" <<'PY'
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
    python3 - "$dn" "$total" "$counts" "$avg_s" "$last_s" "$cur" <<'PY'
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
sys.stdout.write("%s %s %s  avg %.1f мин  ETA %s%s\n"
                 % (ts, pct, cnt, avg / 60.0, eta, tail))
PY
}

# в tty — каждая строка; в pipe/логе — каждые STEP готовых мутантов (иначе шум)
render_throttled() {
    local dn
    dn="$(cut -f1 <<<"$(python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1])).get("outcomes",[])))' "$OUTCOMES" 2>/dev/null || echo 0)")"
    if [ -t 1 ]; then render; return 0; fi
    if [ "$(( dn - LAST_PRINTED ))" -ge "$STEP" ]; then
        LAST_PRINTED="$dn"
        render
    fi
}

# ------------------------------------------------------------------- цикл
if [ "$ONCE" = "1" ]; then
    if pgrep -x cargo-mutants >/dev/null; then echo "(прогон идёт)"; fi
    render
    exit 0
fi

while pgrep -x cargo-mutants >/dev/null; do
    render_throttled
    if command -v inotifywait >/dev/null 2>&1; then
        inotifywait -q -t "$INTERVAL" -e close_write,moved_to "$OUTCOMES" >/dev/null 2>&1 || true
    else
        sleep "$INTERVAL"
    fi
done

render
