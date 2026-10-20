#!/usr/bin/env bash
# tools/mutants-watch.sh — event-based watcher прогона cargo-mutants.
#
# Зачем: аудит зоны/ядра идёт десятки минут; `sleep`-петли в сессии слепы
# (не видно, сколько осталось) и дороги (каждый poll = отдельный turn).
# Скрипт ждёт СОБЫТИЕ (inotify: outcomes.json перезаписан / процесс
# завершился), а не по таймеру, и рисует ОДНУ строку прогресса (как
# hyperfine/nextest): бар с числами done/total внутри, счётчики исходов,
# среднее на мутант, ETA, текущий мутант.
#
# Счётчики: ✓ caught (пойман) · ✗ missed (выжил — дыра в тестах) ·
#           ⏱ timeout (повес) · ⊘ unviable (не компилится — дыра в покрытии)
#
# Использование:
#   tools/mutants-watch.sh                 # ждать финиша прогона c прогрессом
#   tools/mutants-watch.sh --once          # один снимок и выйти
#   tools/mutants-watch.sh --interval 3    # тик-страховка, сек (по умолчанию 5)
#   MUTANTS_LOG=/tmp/mut.log tools/…       # откуда читать "Found N mutants to test"
#   NO_COLOR=1                              # без ANSI; в не-tty — троттлинг вывода
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

while [ $# -gt 0 ]; do
    case "$1" in
        --once)     ONCE=1; shift ;;
        --interval) INTERVAL="$2"; shift 2 ;;
        --step)     STEP="$2"; shift 2 ;;
        -h|--help)  sed -n '2,32p' "$0"; exit 0 ;;
        *)          echo "unknown arg: $1" >&2; exit 64 ;;
    esac
done

[ -d "$OUT" ] || { echo "нет $OUT — аудит не запускался?" >&2; exit 2; }

COLOR=0
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then COLOR=1; fi
export COLOR

# текущий мутант — самый свежий .diff (появился во время билда/теста)
current() {
    local f n
    f="$(find "$DIFFDIR" -maxdepth 1 -name '*.diff' -printf '%T@ %p\n' 2>/dev/null \
        | sort -rn | head -1 | cut -d' ' -f2- || true)"
    [ -n "$f" ] || return 0
    n="$(basename -- "${f##*/}" .diff)"
    n="${n%%_col_*}"
    n="${n#devharness_src_}"
    n="${n//__//}"
    n="$(printf '%s' "$n" | sed -E 's/_line_([0-9]+)$/:\1/')"
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
    python3 - "$dn" "$total" "$counts" "$avg_s" "$last_s" "$cur" <<'PY'
import os, sys, time
dn, total, counts_s, avg_s, last, cur = sys.argv[1:7]
dn = int(dn); total = int(total); avg = float(avg_s)
counts = eval(counts_s) if counts_s else {}
color = os.environ.get("COLOR") == "1"
C = {"ok": "\033[32m", "bad": "\033[1;31m", "warn": "\033[33m",
     "info": "\033[36m", "dim": "\033[2m", "off": "\033[0m"} if color else \
    {k: "" for k in ("ok", "bad", "warn", "info", "dim", "off")}
W = 28
filled = min(W, dn * W // total) if total else 0
label = "%d/%d" % (dn, total)
pad = max(0, (W - len(label)) // 2)
inner = (" " * pad + label + " " * (W - len(label) - pad))[:W]
BG = "\033[42m"; FG = "\033[1;97m"; RESET = "\033[0m"
if not color:
    BG = FG = RESET = ""


def seg(text, on):
    body = ""
    for ch in text:
        if ch == " ":
            body += "\u2588" if on else "\u2591"
        elif on:
            body += FG + ch + RESET + BG
        else:
            body += ch
    if on:
        return BG + body + RESET
    return C["dim"] + body + C["off"]


bar = seg(inner[:filled], True) + seg(inner[filled:], False)

eta = "n/a"
if total and dn >= total:
    eta = C["ok"] + "готово" + C["off"]
elif avg > 0:
    eta = C["info"] + "%.1f мин" % (avg * (total - dn) / 60.0) + C["off"]
cnt = []
if counts.get("CaughtMutant", 0) > 0:
    cnt.append("%s\u2713%s" % (C["ok"], C["off"]))
for k, sym_c, col in (("MissedMutant", "\u2717", "bad"),
                      ("TimeoutMutant", "\u23f1", "warn"),
                      ("Unviable", "\u2298", "warn")):
    cnt.append("%s%s %d%s" % (C[col], sym_c, counts.get(k, 0), C["off"]))
cnt = "  ".join(cnt)
ts = C["dim"] + time.strftime("%H:%M:%S") + C["off"]
tail = ""
if last:
    tail += "  " + C["dim"] + last + C["off"]
if cur:
    tail += "  " + C["warn"] + "\u25b8 " + cur + C["off"]
sys.stdout.write("%s  %s  %s  avg %.1f мин  ETA %s%s\n" % (ts, bar, cnt, avg / 60.0, eta, tail))
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
