#!/usr/bin/env python3
"""Сводка/сверка вердиктов cargo-mutants (приёмка профиля мутантов, §9.28).

Использование:
    scripts/mutants-accept-report.py mutants.out/outcomes.json
    scripts/mutants-accept-report.py mutants-accept-old.json mutants-accept-new.json

Первый режим печатает сводку по одному прогону, второй — сверяет два прогона
по ИМЕНИ мутанта и печатает расхождения. Exit code: 0 — расхождений нет,
1 — есть (для CI), 2 — ошибка входных данных.
"""
from __future__ import annotations

import json
import sys
from collections import Counter
from pathlib import Path

VERDICT_BY_SUMMARY = {
    "CaughtMutant": "caught",
    "MissedMutant": "missed",
    "Timeout": "timeout",
    "Unviable": "unviable",
}


def load(path: str) -> dict[str, str]:
    raw = json.loads(Path(path).read_text(encoding="utf-8", errors="replace"))
    outcomes = raw.get("outcomes", raw if isinstance(raw, list) else [])
    out: dict[str, str] = {}
    for item in outcomes:
        scenario = item.get("scenario", {})
        mutant = scenario.get("Mutant") or {}
        name = mutant.get("name") or item.get("log_path", "?")
        summary = item.get("summary", "Unknown")
        out[name] = VERDICT_BY_SUMMARY.get(summary, summary)
    return out


def summarize(path: str) -> tuple[dict[str, str], Counter]:
    verdicts = load(path)
    return verdicts, Counter(verdicts.values())


def main(argv: list[str]) -> int:
    if len(argv) == 2:
        verdicts, counts = summarize(argv[1])
        total = sum(counts.values())
        print(f"прогон: {argv[1]}")
        print(f"  мутантов: {total}  " + "  ".join(f"{k}={counts[k]}" for k in sorted(counts)))
        testable = total - counts["unviable"]
        if testable:
            print(f"  kill-rate по testable: {counts['caught']}/{testable} = {counts['caught'] * 100 / testable:.1f}%")
        for name, verdict in sorted(verdicts.items()):
            if verdict != "caught":
                print(f"  {verdict:8s} {name}")
        return 0

    if len(argv) == 3:
        old_path, new_path = argv[1], argv[2]
        old, old_counts = summarize(old_path)
        new, new_counts = summarize(new_path)
        print(f"сверка вердиктов: {old_path} vs {new_path}")
        print(f"  old: {dict(old_counts)}")
        print(f"  new: {dict(new_counts)}")
        diffs: list[str] = []
        for name in sorted(set(old) | set(new)):
            o, n = old.get(name, "<нет>"), new.get(name, "<нет>")
            if o != n:
                diffs.append(f"  {name}: {o} -> {n}")
        if diffs:
            print(f"РАСХОЖДЕНИЙ: {len(diffs)}")
            print("\n".join(diffs))
            return 1
        print("РАСХОЖДЕНИЙ НЕТ: вердикты совпадают по всем мутантам")
        return 0

    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
