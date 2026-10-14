#!/usr/bin/env bash
# devharness/selfmutate.sh — мутационная проверка СОБСТВЕННЫХ тестов харнесса.
# Вносим известные баги в ИЗОЛИРОВАННУЮ копию devh и проверяем, что test.sh их убивает.
# «Тесты на вес золота» — применимо и к тестам самого харнесса (kill-rate).
# Гейт: 13/13. Новые гейты devh должны сопровождаться новым мутантом здесь.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
W="$(mktemp -d /tmp/devh-selfmut-XXXXXX)"
trap 'rm -rf "$W"' EXIT
cp -r "$HERE/." "$W/h"
cp "$W/h/devh" "$W/h/devh.orig"
KILLED=0; TOTAL=0; SKIP=0

# kill-rate валиден ТОЛЬКО на зелёном базовом (иначе убит «любой» мутант)
if ! bash "$W/h/test.sh" >/dev/null 2>&1; then
  echo "❌ базовый suite на чистой копии КРАСНЫЙ — kill-rate невалиден, стоп"
  exit 1
fi
echo "базовый suite на чистой копии: зелёный (kill-rate будет валиден)"

mutant() { # $1=имя, $2=py-файл патча
  local name="$1" patch="$2"
  TOTAL=$((TOTAL+1))
  cp "$W/h/devh.orig" "$W/h/devh"
  if ! (cd "$W/h" && python3 "$patch") 2>"$W/err"; then
    echo "  SKIP $name (якорь мутанта устарел — обнови selfmutate.sh)"; SKIP=$((SKIP+1)); return
  fi
  bash -n "$W/h/devh" || { echo "  SKIP $name (сломан синтаксис — невалидный мутант)"; SKIP=$((SKIP+1)); return; }
  if bash "$W/h/test.sh" >/dev/null 2>&1; then
    echo "  ❌ ВЫЖИЛ: $name — suite НЕ ловит этот баг"
  else
    echo "  ✓ убит: $name"; KILLED=$((KILLED+1))
  fi
}

cat > "$W/m1.py" <<'P1'
s = open('devh', encoding='utf-8').read()
o = '''    if [ "$NO_IGNORED" = 1 ] && ! no_ignored_ok "$log"; then which=NO-IGNORED; return 1; fi'''
n = '''    if false; then which=NO-IGNORED; return 1; fi'''
assert s.count(o) == 1
open('devh', 'w', encoding='utf-8').write(s.replace(o, n))
P1
cat > "$W/m2.py" <<'P2'
s = open('devh', encoding='utf-8').read()
o = '''  if [ -n "$MSG_DENY" ] && printf '%s\\n' "$msg" | grep -qiE "$MSG_DENY"; then
    echo "FAIL: сообщение коммита нарушает гигиену ($MSG_DENY): «$msg»" >&2'''
n = '''  if false && printf '%s\\n' "$msg" | grep -qiE "$MSG_DENY"; then
    echo "FAIL: сообщение коммита нарушает гигиену ($MSG_DENY): «$msg»" >&2'''
assert s.count(o) == 1
open('devh', 'w', encoding='utf-8').write(s.replace(o, n))
P2
cat > "$W/m3.py" <<'P3'
s = open('devh', encoding='utf-8').read()
o = '''    if [ "$na" -ge "$MAX_ACTIVE" ]; then'''
n = '''    if [ "$na" -lt 0 ]; then'''
assert s.count(o) == 1
open('devh', 'w', encoding='utf-8').write(s.replace(o, n))
P3
cat > "$W/m4.py" <<'P4'
s = open('devh', encoding='utf-8').read()
o = '''    echo "OK resume worktree: $wt"
    fresh=0'''
n = '''    echo "OK resume worktree: $wt"'''
assert s.count(o) == 1
open('devh', 'w', encoding='utf-8').write(s.replace(o, n))
P4
cat > "$W/m5.py" <<'P5'
s = open('devh', encoding='utf-8').read()
o = '''  if [ -f "$d/red.cmd" ] && [ ! -s "$d/no-red" ]; then'''
n = '''  if false; then'''
assert s.count(o) == 1
open('devh', 'w', encoding='utf-8').write(s.replace(o, n))
P5
cat > "$W/m6.py" <<'P6'
s = open('devh', encoding='utf-8').read()
o = '''  if [ -n "$MSG_DENY" ] && printf '%s\\n' "$msg" | grep -qiE "$MSG_DENY"; then
    echo "FAIL: сообщение всё ещё нарушает гигиену: «$msg» — правь $sum" >&2; exit 1'''
n = '''  if false; then
    echo "FAIL: сообщение всё ещё нарушает гигиену: «$msg» — правь $sum" >&2; exit 1'''
assert s.count(o) == 1
open('devh', 'w', encoding='utf-8').write(s.replace(o, n))
P6
cat > "$W/m7.py" <<'P7'
s = open('devh', encoding='utf-8').read()
o = '''  if [ "$(git -C "$wt" rev-list --count "$MAIN..HEAD")" -eq 0 ] 2>/dev/null; then'''
n = '''  if [ "$(git -C "$wt" rev-list --count "$MAIN..HEAD")" -eq 99 ] 2>/dev/null; then'''
assert s.count(o) == 1
open('devh', 'w', encoding='utf-8').write(s.replace(o, n))
P7
cat > "$W/m8.py" <<'P8'
s = open('devh', encoding='utf-8').read()
o = '''        if [ "$pct" -lt "$MIN_KILL" ]; then
          echo "mutation FAIL $f: kill-rate'''
n = '''        if false; then
          echo "mutation FAIL $f: kill-rate'''
assert s.count(o) == 1
open('devh', 'w', encoding='utf-8').write(s.replace(o, n))
P8
cat > "$W/m9.py" <<'P9'
s = open('devh', encoding='utf-8').read()
o = '''  elif [ ! -f "$d/red.cmd" ]; then'''
n = '''  elif false; then'''
assert s.count(o) == 1
open('devh', 'w', encoding='utf-8').write(s.replace(o, n))
P9
cat > "$W/m10.py" <<'P10'
s = open('devh', encoding='utf-8').read()
o = '''        timeout "$AGENT_TIMEOUT" $AGENT "$1" ) > "$HDIR/logs/$id-agent.log" 2>&1 \\'''
n = '''        timeout 300 $AGENT "$1" ) > "$HDIR/logs/$id-agent.log" 2>&1 \\'''
assert s.count(o) == 1
open('devh', 'w', encoding='utf-8').write(s.replace(o, n))
P10
cat > "$W/m11.py" <<'P11'
s = open('devh', encoding='utf-8').read()
o = '''    flock -n 8 || { echo "devh: $1 сейчас в работе (run.lock занят) — $2 отклонён" >&2; exit 1; }'''
n = '''    flock -x 8 || { echo "devh: $1 сейчас в работе (run.lock занят) — $2 отклонён" >&2; exit 1; }'''
assert s.count(o) == 1
open('devh', 'w', encoding='utf-8').write(s.replace(o, n))
P11
cat > "$W/m12.py" <<'P12'
s = open('devh', encoding='utf-8').read()
o = '''  if head -1 "$sum" | grep -q '^BLOCKED'; then'''
n = '''  if false; then'''
assert s.count(o) == 1
open('devh', 'w', encoding='utf-8').write(s.replace(o, n))
P12
cat > "$W/m13.py" <<'P13'
s = open('devh', encoding='utf-8').read()
o = '''        [ "$pct" -lt "$MIN_KILL" ] && { echo "mutation FAIL: kill-rate ${pct}% < ${MIN_KILL}%"; return 1; }'''
n = '''        false && { echo "mutation FAIL: kill-rate ${pct}% < ${MIN_KILL}%"; return 1; }'''
assert s.count(o) == 1
open('devh', 'w', encoding='utf-8').write(s.replace(o, n))
P13

echo "Мутационная проверка тестов devh (изолированная копия: $W/h):"
mutant "M1: NO-IGNORED гейт отключён" "$W/m1.py"
mutant "M2: гигиена сообщения (MSG_DENY) отключена" "$W/m2.py"
mutant "M3: WIP-лимит отключён" "$W/m3.py"
mutant "M4: resume ломает RED pre-check (старый баг)" "$W/m4.py"
mutant "M5: RED пост-гейт отключён (позеленение не проверяется)" "$W/m5.py"
mutant "M6: гигиена в devh commit отключена (2-я линия)" "$W/m6.py"
mutant "M7: merge-guard «нет коммитов» отключён" "$W/m7.py"
mutant "M8: kill-rate MIN_KILL не проверяется (мутации не гейтят)" "$W/m8.py"
mutant "M9: обязательность RED отключена (нет red.cmd → run идёт)" "$W/m9.py"
mutant "M10: watchdog-timeout игнорируется (всегда 300с)" "$W/m10.py"
mutant "M11: run.lock блокирующий вместо отказного (гонка merge/run)" "$W/m11.py"
mutant "M12: BLOCKED-детект отключён" "$W/m12.py"
mutant "M13: kill-rate в single-run ветке не проверяется" "$W/m13.py"
echo
echo "KILL-RATE: $KILLED/$TOTAL (skip=$SKIP)"
[ "$KILLED" -eq "$TOTAL" ] && [ "$SKIP" -eq 0 ]
