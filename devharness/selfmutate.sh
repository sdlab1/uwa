#!/usr/bin/env bash
# devharness/selfmutate.sh — мутационная проверка СОБСТВЕННЫХ тестов харнесса.
# Вносим известные баги в ИЗОЛИРОВАННУЮ копию devh и проверяем, что test.sh их убивает.
# «Тесты на вес золота» — применимо и к тестам самого харнесса (kill-rate).
# Гейт: 4/4. Новые гейты devh должны сопровождаться новым мутантом здесь.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
W="$(mktemp -d /tmp/devh-selfmut-XXXXXX)"
trap 'rm -rf "$W"' EXIT
cp -r "$HERE/." "$W/h"
cp "$W/h/devh" "$W/h/devh.orig"
KILLED=0; TOTAL=0; SKIP=0

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

echo "Мутационная проверка тестов devh (изолированная копия: $W/h):"
mutant "M1: NO-IGNORED гейт отключён" "$W/m1.py"
mutant "M2: гигиена сообщения (MSG_DENY) отключена" "$W/m2.py"
mutant "M3: WIP-лимит отключён" "$W/m3.py"
mutant "M4: resume ломает RED pre-check (старый баг)" "$W/m4.py"
echo
echo "KILL-RATE: $KILLED/$TOTAL (skip=$SKIP)"
[ "$KILLED" -eq "$TOTAL" ] && [ "$SKIP" -eq 0 ]
