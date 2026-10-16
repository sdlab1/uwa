#!/usr/bin/env bash
# devharness/selfmutate.sh — мутационная проверка СОБСТВЕННЫХ тестов харнесса.
# Вносим известные баги в ИЗОЛИРОВАННУЮ копию devh-classic и проверяем, что test.sh их убивает.
# «Тесты на вес золота» — применимо и к тестам самого харнесса.
# Патчер — чистый awk (ENVIRON, без escape-ловушек -v); python-зависимости нет.
# Гейт: 18/18. Новые гейты devh должны сопровождаться новым мутантом здесь.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
W="$(mktemp -d /tmp/devh-selfmut-XXXXXX)"
trap 'rm -rf "$W"' EXIT
cp -r "$HERE/." "$W/h"
cp "$W/h/devh-classic" "$W/h/devh.orig"
KILLED=0; TOTAL=0; SKIP=0

# kill-rate валиден ТОЛЬКО на зелёном базовом (иначе убит «любой» мутант)
if ! DEVH="$W/h/devh-classic" bash "$W/h/test.sh" >/dev/null 2>&1; then
  echo "❌ базовый suite на чистой копии КРАСНЫЙ — kill-rate невалиден, стоп"
  exit 1
fi
echo "базовый suite на чистой копии: зелёный (kill-rate будет валиден)"

# awk-патчер: заменяет ПЕРВОЕ точное многострочное вхождение old → new в $W/h/devh-classic.
# Значения — через ENVIRON (в отличие от -v, НЕ обрабатывают escape-последовательности).
apply_mutant() { # $1=old-файл $2=new-файл; rc≠0 — якорь не найден
  OF="$(cat "$1")" NF_="$(cat "$2")" awk '
    BEGIN { n = split(ENVIRON["OF"], ol, "\n") }
    { buf[NR] = $0 }
    END {
      replaced = 0
      for (i = 1; i <= NR; i++) {
        if (!replaced && n > 0 && buf[i] == ol[1] && i + n - 1 <= NR) {
          ok = 1
          for (j = 2; j <= n; j++) if (buf[i+j-1] != ol[j]) { ok = 0; break }
          if (ok) { printf "%s\n", ENVIRON["NF_"]; i += n - 1; replaced = 1; continue }
        }
        print buf[i]
      }
      exit replaced ? 0 : 1
    }' "$W/h/devh-classic" > "$W/h/devh.new" && mv -f "$W/h/devh.new" "$W/h/devh-classic"
}

mutant() { # $1=имя $2=old $3=new
  local name="$1"
  TOTAL=$((TOTAL+1))
  cp "$W/h/devh.orig" "$W/h/devh-classic"
  if ! apply_mutant "$2" "$3"; then
    echo "  SKIP $name (якорь мутанта устарел — обнови selfmutate.sh)"; SKIP=$((SKIP+1)); return
  fi
  bash -n "$W/h/devh-classic" || { echo "  SKIP $name (сломан синтаксис — невалидный мутант)"; SKIP=$((SKIP+1)); return; }
  if DEVH="$W/h/devh-classic" bash "$W/h/test.sh" >/dev/null 2>&1; then
    echo "  ❌ ВЫЖИЛ: $name — suite НЕ ловит этот баг"
  else
    echo "  ✓ убит: $name"; KILLED=$((KILLED+1))
  fi
}

# ─── M1: NO-IGNORED гейт отключён ───
cat > "$W/m.old" <<'S'
    if [ "$NO_IGNORED" = 1 ] && ! no_ignored_ok "$log"; then which=NO-IGNORED; return 1; fi
S
cat > "$W/m.new" <<'S'
    if false; then which=NO-IGNORED; return 1; fi
S
mutant "M1: NO-IGNORED гейт отключён" "$W/m.old" "$W/m.new"

# ─── M2: гигиена сообщения (run) отключена ───
cat > "$W/m.old" <<'S'
  if [ -n "$MSG_DENY" ] && printf '%s\n' "$msg" | grep -qiE "$MSG_DENY"; then
    echo "${C_R}FAIL: сообщение коммита нарушает гигиену ($MSG_DENY): «$msg»${C_0}" >&2
S
cat > "$W/m.new" <<'S'
  if false && printf '%s\n' "$msg" | grep -qiE "$MSG_DENY"; then
    echo "${C_R}FAIL: сообщение коммита нарушает гигиену ($MSG_DENY): «$msg»${C_0}" >&2
S
mutant "M2: гигиена сообщения (MSG_DENY) отключена" "$W/m.old" "$W/m.new"

# ─── M3: WIP-лимит отключён ───
cat > "$W/m.old" <<'S'
    if [ "$na" -ge "$MAX_ACTIVE" ]; then
S
cat > "$W/m.new" <<'S'
    if [ "$na" -lt 0 ]; then
S
mutant "M3: WIP-лимит отключён" "$W/m.old" "$W/m.new"

# ─── M4: resume ломает RED pre-check (старый баг) ───
cat > "$W/m.old" <<'S'
    echo "${C_G}OK resume worktree: $wt${C_0}"
    fresh=0
S
cat > "$W/m.new" <<'S'
    echo "${C_G}OK resume worktree: $wt${C_0}"
S
mutant "M4: resume ломает RED pre-check (старый баг)" "$W/m.old" "$W/m.new"

# ─── M5: RED пост-гейт отключён ───
cat > "$W/m.old" <<'S'
  if [ -f "$d/red.cmd" ] && [ ! -s "$d/no-red" ]; then
S
cat > "$W/m.new" <<'S'
  if false; then
S
mutant "M5: RED пост-гейт отключён (позеленение не проверяется)" "$W/m.old" "$W/m.new"

# ─── M6: гигиена в devh commit отключена (2-я линия) ───
cat > "$W/m.old" <<'S'
  if [ -n "$MSG_DENY" ] && printf '%s\n' "$msg" | grep -qiE "$MSG_DENY"; then
    echo "${C_R}FAIL: сообщение всё ещё нарушает гигиену: «$msg» — правь $sum${C_0}" >&2; exit 1
S
cat > "$W/m.new" <<'S'
  if false; then
    echo "${C_R}FAIL: сообщение всё ещё нарушает гигиену: «$msg» — правь $sum${C_0}" >&2; exit 1
S
mutant "M6: гигиена в devh commit отключена (2-я линия)" "$W/m.old" "$W/m.new"

# ─── M7: merge-guard «нет коммитов» отключён ───
cat > "$W/m.old" <<'S'
  if [ "$(git -C "$wt" rev-list --count "$MAIN..HEAD")" -eq 0 ] 2>/dev/null; then
S
cat > "$W/m.new" <<'S'
  if [ "$(git -C "$wt" rev-list --count "$MAIN..HEAD")" -eq 99 ] 2>/dev/null; then
S
mutant "M7: merge-guard «нет коммитов» отключён" "$W/m.old" "$W/m.new"

# ─── M8: kill-rate (per-file) не проверяется ───
cat > "$W/m.old" <<'S'
        if [ "$pct" -lt "$MIN_KILL" ]; then
          echo "mutation FAIL $f: kill-rate ${pct}% < ${MIN_KILL}% — тесты не ловят мутантов"
S
cat > "$W/m.new" <<'S'
        if false; then
          echo "mutation FAIL $f: kill-rate ${pct}% < ${MIN_KILL}% — тесты не ловят мутантов"
S
mutant "M8: kill-rate MIN_KILL не проверяется (per-file)" "$W/m.old" "$W/m.new"

# ─── M9: обязательность RED отключена ───
cat > "$W/m.old" <<'S'
  elif [ ! -f "$d/red.cmd" ]; then
S
cat > "$W/m.new" <<'S'
  elif false; then
S
mutant "M9: обязательность RED отключена (нет red.cmd → run идёт)" "$W/m.old" "$W/m.new"

# ─── M10: watchdog-timeout игнорируется ───
cat > "$W/m.old" <<'S'
        timeout "$AGENT_TIMEOUT" $AGENT "$1" ) > "$HDIR/logs/$id-agent.log" 2>&1 \
S
cat > "$W/m.new" <<'S'
        timeout 300 $AGENT "$1" ) > "$HDIR/logs/$id-agent.log" 2>&1 \
S
mutant "M10: watchdog-timeout игнорируется (всегда 300с)" "$W/m.old" "$W/m.new"

# ─── M11: run.lock блокирующий вместо отказного ───
cat > "$W/m.old" <<'S'
    flock -n 8 || { echo "devh: $1 сейчас в работе (run.lock занят) — $2 отклонён" >&2; exit 1; }
S
cat > "$W/m.new" <<'S'
    flock -x 8 || { echo "devh: $1 сейчас в работе (run.lock занят) — $2 отклонён" >&2; exit 1; }
S
mutant "M11: run.lock блокирующий вместо отказного (гонка merge/run)" "$W/m.old" "$W/m.new"

# ─── M12: BLOCKED-детект отключён ───
cat > "$W/m.old" <<'S'
  if head -1 "$sum" | grep -q '^BLOCKED'; then
S
cat > "$W/m.new" <<'S'
  if false; then
S
mutant "M12: BLOCKED-детект отключён" "$W/m.old" "$W/m.new"

# ─── M13: kill-rate (single-run) не проверяется ───
cat > "$W/m.old" <<'S'
        [ "$pct" -lt "$MIN_KILL" ] && { echo "mutation FAIL: kill-rate ${pct}% < ${MIN_KILL}%"; return 1; }
S
cat > "$W/m.new" <<'S'
        false && { echo "mutation FAIL: kill-rate ${pct}% < ${MIN_KILL}%"; return 1; }
S
mutant "M13: kill-rate в single-run ветке не проверяется" "$W/m.old" "$W/m.new"

# ─── M14: LINT-гейт инвертирован ───
cat > "$W/m.old" <<'S'
      if ! shellcheck "$wt/$f"; then bad=1; fi
S
cat > "$W/m.new" <<'S'
      if shellcheck "$wt/$f" >/dev/null 2>&1; then bad=1; fi
S
mutant "M14: LINT-гейт инвертирован (грязный код проходит, чистый падает)" "$W/m.old" "$W/m.new"

# ─── M15: REVIEW-вердикт ISSUES не матчится ───
cat > "$W/m.old" <<'S'
    *"VERDICT: ISSUES"*) return 1 ;;
S
cat > "$W/m.new" <<'S'
    *"VERDICT: NEVER"*) return 1 ;;
S
mutant "M15: REVIEW-вердикт ISSUES не матчится (замечания ревьюера игнорируются)" "$W/m.old" "$W/m.new"

# ─── M16: auto игнорирует обязательность RED ───
cat > "$W/m.old" <<'S'
    if [ ! -f "$(items_dir)/$item/red.cmd" ] && [ ! -s "$(items_dir)/$item/no-red" ]; then
S
cat > "$W/m.new" <<'S'
    if false; then
S
mutant "M16: auto не требует RED-пруф (спека без RED уезжает в прогон)" "$W/m.old" "$W/m.new"

# ─── M17: CRAP-гейт отключён ───
cat > "$W/m.old" <<'S'
    if [ -n "$CRAP" ] && [ ! -s "$d/no-crap" ]; then
      crap_gate > "$craplog" 2>&1
S
cat > "$W/m.new" <<'S'
    if false; then
      crap_gate > "$craplog" 2>&1
S
mutant "M17: CRAP-гейт отключён (нарушения пропускаются)" "$W/m.old" "$W/m.new"

# ─── M18: сравнение score с CRAP_MAX инвертировано ───
cat > "$W/m.old" <<'S'
    if [ "$s_x1000" -gt $(( CRAP_MAX * 1000 )) ]; then
S
cat > "$W/m.new" <<'S'
    if [ "$s_x1000" -le $(( CRAP_MAX * 1000 )) ]; then
S
mutant "M18: score > MAX инвертировано (зелёное краснеет, красное зеленеет)" "$W/m.old" "$W/m.new"

echo
echo "KILL-RATE: $KILLED/$TOTAL (skip=$SKIP)"
[ "$KILLED" -eq "$TOTAL" ] && [ "$SKIP" -eq 0 ]
