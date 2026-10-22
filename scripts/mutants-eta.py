#!/usr/bin/env python3
"""ETA по текущему прогону cargo-mutants (по mutants.out/log + outcomes.json).

    scripts/mutants-eta.py [каталог_прогона]   # по умолчанию .

Показывает: сколько мутантов в прогоне, сколько сделано, среднее на мутанта,
ETA до конца и список непойманных (missed/unviable/timeout) на текущий момент.
"""
from __future__ import annotations

import calendar
import json
import os
import re
import sys
import time
from pathlib import Path

root = Path(sys.argv[1] if len(sys.argv) > 1 else ".").resolve()
out = root / "mutants.out"
log_dir = out / "log"
now = time.time()

done = len(list(log_dir.glob("*.log"))) if log_dir.is_dir() else 0

total = 0
started = ended = None
verdicts: dict[str, int] = {}
missed: list[str] = []
if (out / "outcomes.json").is_file():
    try:
        raw = json.loads((out / "outcomes.json").read_text(encoding="utf-8", errors="replace"))
        items = raw.get("outcomes", [])
        total = raw.get("total_mutants") or len(items)
        for key in ("start_time", "end_time"):
            val = raw.get(key)
            if not val:
                continue
            ts = calendar.timegm(time.strptime(val[:19], "%Y-%m-%dT%H:%M:%S"))
            if key == "start_time":
                started = ts
            else:
                ended = ts
        for item in items:
            summary = item.get("summary", "Unknown")
            verdicts[summary] = verdicts.get(summary, 0) + 1
            if summary != "CaughtMutant":
                name = (item.get("scenario", {}).get("Mutant") or {}).get("name", "?")
                missed.append(f"{summary}: {name}")
    except Exception as exc:  # noqa: BLE001
        print(f"не читаю outcomes.json: {exc}")

if not total and done:
    total = done
# реальное число мутантов — из лога прогона ("Found N mutants to test")
for log_name in ("mutants-accept-new.log", "mutants-accept-old.log", "accept-run.log"):
    cand = root / log_name
    if not cand.is_file():
        continue
    try:
        found = re.search(r"Found (\d+) mutants to test", cand.read_text(encoding="utf-8", errors="replace"))
        if found:
            total = max(total, int(found.group(1)))
            break
    except OSError:
        pass
first_log = min((p.stat().st_mtime for p in log_dir.glob("*.log")), default=None)
if started is None:
    started = first_log

print(f"прогон: {root}")
print(f"  мутантов всего: {total}   сделано: {done}")
if verdicts:
    print("  вердикты: " + "  ".join(f"{k}={v}" for k, v in sorted(verdicts.items())))
elapsed = (now - started) if started else 0
if done and elapsed > 0:
    per = elapsed / done
    left = max(0, total - done)
    eta = now + left * per
    print(f"  прошло: {elapsed / 60:.1f} мин   на мутанта: {per:.1f} с")
    print(f"  осталось: {left}   ETA: {time.strftime('%H:%M', time.localtime(eta))} "
          f"(+{left * per / 60:.0f} мин)")
if missed:
    print(f"  непойманных на сейчас: {len(missed)}")
    for line in missed[-15:]:
        print(f"    {line}")
