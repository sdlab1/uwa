#!/usr/bin/env python3
"""Починить зависимости пересозданных задач (revoke+re-add) и убрать ошибочно воскрешённые."""
import json, re, subprocess, sys
D = "/home/user/project_uwa/target/debug/devh"
REG = "/home/user/project_uwa/.harness/orch/registry.json"
MAPF = "/home/user/project_uwa/.harness/revoked_map.json"

reg = json.load(open(REG))
mapping = json.load(open(MAPF))
tasks = {t["id"]: t for t in reg["tasks"]}
plan = reg.setdefault("plan", {})
prio = plan.setdefault("prio", {})
deps = plan.setdefault("deps", {})

# (a) три задачи, воскрешённые по ошибке (были отозваны ранее решением lead) — возвращаем как было
for tid in ("T-195", "T-196", "T-202"):
    if tid in tasks and tasks[tid]["status"] == "queued":
        subprocess.run([D, "orch", "revoke", tid, "--reason", "возвращено к прежнему решению lead (было отозвано до инцидента deepseek)"],
                       capture_output=True, text=True)
        for old, new in list(mapping.items()):
            if new == tid:
                mapping.pop(old)
        print("revoke-дубликат:", tid)

def readd(old_id):
    """Пересоздать задачу old_id с корректными deps (по mapping) и вернуть новый ID."""
    t = tasks[old_id]
    args = [D, "orch", "add", "--agent", t.get("agent"), "--p", str(t.get("complexity", 2)),
            "--title", t["title"]]
    if t.get("c_level") is not None:
        args += ["--c", str(t["c_level"])]
    if old_id in prio:
        args += ["--prio", str(prio[old_id])]
    for z in t.get("zones", []):
        args += ["--zone", z]
    for d in t.get("dod", []):
        args += ["--dod", d]
    for c in t.get("checks", []):
        args += ["--check", c]
    want = []
    for d in deps.get(old_id, []):
        nd = mapping.get(d, d)
        if nd in tasks or nd in mapping.values() or nd in {x["id"] for x in reg["tasks"]}:
            want.append(nd)
    for nd in want:
        args += ["--dep", nd]
    if prio.get(old_id, 9) == 0:
        args += ["--notify"]
    out = subprocess.run(args, capture_output=True, text=True).stdout
    m = re.search(r"(T-\d+) добавлена", out)
    if not m:
        print("FAIL", old_id, out.strip()[:120], file=sys.stderr)
        return None
    return m.group(1)

for _ in range(4):
    changed = False
    for old in sorted(mapping, key=lambda x: int(x.split("-")[1])):
        new = mapping[old]
        if new not in tasks:
            continue
        want = []
        for d in deps.get(old, []):
            nd = mapping.get(d, d)
            if nd not in want:
                want.append(nd)
        cur = deps.get(new, [])
        if set(want) == set(cur):
            continue
        subprocess.run([D, "orch", "revoke", new, "--reason", "пересоздание с восстановленными зависимостями"],
                       capture_output=True, text=True)
        nn = readd(old)
        if nn:
            mapping[old] = nn
            changed = True
            print(f"  {old} {new} -> {nn} deps={want}")
    if not changed:
        break

json.dump(mapping, open(MAPF, "w"), indent=1)
reg2 = json.load(open(REG))
d2 = (reg2.get("plan", {}) or {}).get("deps", {}) or {}
print("--- итоговые зависимости (критичные) ---")
for k in sorted(d2, key=lambda x: int(x.split("-")[1])):
    if int(k.split("-")[1]) >= 195:
        print(" ", k, "<-", d2[k])
