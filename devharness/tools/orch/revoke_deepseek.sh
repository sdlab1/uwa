#!/usr/bin/env bash
# deepseek/opencode накрылся: отзыв всех его задач + перераспределение + чистка зависших.
set -u
D=/home/user/project_uwa/target/debug/devh
cd /home/user/project_uwa || exit 1
R=.harness/orch/registry.json

REV="deepseek-4.1 (opencode-0a0e0cfc) недоступен: рантайм накрылся 2026-10-24 13:02"

# 1) отзыв всех незавершённых задач opencode
for id in $(python3 - <<'PY'
import json
r=json.load(open('.harness/orch/registry.json'))
print(' '.join(t['id'] for t in r['tasks'] if t.get('agent')=='opencode-0a0e0cfc' and t['status']!='done'))
PY
); do
  $D orch revoke "$id" --reason "$REV" >/dev/null 2>&1 || echo "revoke FAIL $id"
done
echo "REVOKED_OPENCODE_DONE"

# 2) перераспределение (зоны разведены по агентам; пересечения — только в файлах оркестратора)
assign() { $D orch reassign "$1" --agent "$2" >/dev/null 2>&1 && echo "  $1 -> $2" || echo "  $1 -> $2 FAILED"; }

# --- beam3 (nemotron C3): гейт качества (новые файлы) + fail-closed commit_sha + зависания + identity
for t in T-190 T-191 T-153 T-172 T-164; do assign $t beam3; done
# --- beam2 (nemotron C3): гейт слияния/коммита + коллизия идентичности + декомпозиция + потолки
for t in T-192 T-165 T-162 T-170; do assign $t beam2; done
# --- kilo: реестр fail-closed + зоны + автослияние
for t in T-169 T-171 T-152; do assign $t kilo-2ba65640; done
# --- beam (laguna C2): модульные C1/C2 задачи
for t in T-141 T-144 T-145 T-146 T-109 T-110 T-167 T-163 T-161; do assign $t beam; done
# --- T-168 (AUTHZ) -> beam2 (C3, отдельные файлы core/authz.rs, shell/msg.rs)
assign T-168 beam2
# --- T-153/T-152 уже в списках выше; T-188-T-189 остаются в буфере lead

# 3) зависшие задачи (активны по 15+ часов, активности нет) -> отзыв
for t in T-075 T-078 T-090 T-093 T-077; do
  $D orch revoke "$t" --reason "зависла без активности >15ч (нет коммитов, нет отчёта) — переоформляется в буфер lead" >/dev/null 2>&1 && echo "  revoked stale $t" || echo "  stale $t skip"
done

# 4) модель kilo — фиксируем известную (иначе потолок по умолчанию)
$D orch model --agent kilo-2ba65640 --model kilo >/dev/null 2>&1 && echo "kilo model=kilo" || echo "kilo model skip"

# 5) итог
python3 - <<'PY'
import json
from collections import Counter
r=json.load(open('.harness/orch/registry.json'))
c=Counter((t.get('agent'),t['status']) for t in r['tasks'])
for a in ['beam','beam2','beam3','kilo-2ba65640','inkling2','opencode-0a0e0cfc','minimax']:
    q=c[(a,'queued')]; act=c[(a,'active')]; d=c[(a,'done')]
    print(f"{a:18} queued={q:2} active={act:2} done={d}")
print("всего задач:",len(r['tasks']))
PY
