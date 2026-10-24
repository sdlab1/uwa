#!/usr/bin/env python3
"""Пересоздать отозванные задачи (deepseek умер) с сохранением DoD/check/зон/приоритетов."""
import json, re, subprocess, sys

D = "/home/user/project_uwa/target/debug/devh"
REG = "/home/user/project_uwa/.harness/orch/registry.json"

reg = json.load(open(REG))
tasks = {t["id"]: t for t in reg["tasks"]}
plan = reg.get("plan", {}) or {}
prio = plan.get("prio", {}) or {}
deps_of = plan.get("deps", {}) or {}

WORKERS = {"beam", "beam2", "beam3", "kilo-2ba65640"}
revoked = [t for t in reg["tasks"] if t["status"] == "revoked"]
# 5 зависших (T-075/078/090/093/077) -> в буфер lead; остальные отозванные -> обратно в работу
STALE = {"T-075", "T-078", "T-090", "T-093", "T-077"}

mapping = {}
created = []
for t in sorted(revoked, key=lambda x: int(x["id"].split("-")[1])):
    old = t["id"]
    if old in STALE:
        agent, buf = "minimax", True
    else:
        agent, buf = t.get("agent"), False
        if agent not in WORKERS:
            continue
    args = [D, "orch", "add", "--agent", agent, "--p", str(t.get("complexity", 2)),
            "--title", t["title"]]
    if buf:
        args += ["--buffer"]
    c = t.get("c_level")
    if c is not None:
        args += ["--c", str(c)]
    if old in prio:
        args += ["--prio", str(prio[old])]
    for z in t.get("zones", []):
        args += ["--zone", z]
    for d in t.get("dod", []):
        args += ["--dod", d]
    for c in t.get("checks", []):
        args += ["--check", c]
    for d in deps_of.get(old, []):
        nd = mapping.get(d, d)
        if nd in tasks or nd in mapping:
            args += ["--dep", nd]
    if prio.get(old, 9) == 0:
        args += ["--notify"]
    out = subprocess.run(args, capture_output=True, text=True).stdout
    m = re.search(r"(T-\d+) добавлена", out)
    if not m:
        print("FAIL", old, out.strip()[:120], file=sys.stderr)
        continue
    new = m.group(1)
    mapping[old] = new
    created.append((old, new, agent, "buffer" if buf else "work", t.get("complexity")))

json.dump(mapping, open("/home/user/project_uwa/.harness/revoked_map.json", "w"), indent=1)
for row in created:
    print("  ", *row)
print("ВСЕГО пересоздано:", len(created))
