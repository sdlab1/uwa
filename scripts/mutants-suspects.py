#!/usr/bin/env python3
"""Собрать список подозрительных вердиктов прогона (env-блокер-Caught + все MISSED).

    scripts/mutants-suspects.py /tmp/accept > suspects.txt

Подозрительный "Caught" = мутант, чей тест упал из-за «мутация env-блокер»
(load-флак внутри integration-тестов, §9.29). Такие вердикты не доказывают поимку.
В список попадают также все непойманные (MISSED/Timeout/Unviable) — их тоже надо
перепроверить с выключенным load-гейтом.
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

run_dir = Path(sys.argv[1] if len(sys.argv) > 1 else ".").resolve()
outcomes = json.loads((run_dir / "mutants.out" / "outcomes.json").read_text(encoding="utf-8", errors="replace"))
log_dir = run_dir / "mutants.out" / "log"

suspects: list[tuple[str, str]] = []  # (имя мутанта, причина)
for item in outcomes.get("outcomes", []):
    mutant = (item.get("scenario") or {}).get("Mutant") or {}
    name = mutant.get("name", "?")
    summary = item.get("summary", "Unknown")
    rel = item.get("log_path")
    if summary == "CaughtMutant" and rel:
        log = log_dir / Path(rel).name
        try:
            text = log.read_text(encoding="utf-8", errors="replace")
        except OSError:
            text = ""
        if "env-блокер" in text or "env-blocked" in text:
            why = "env-блокер (load-флак)" if "env-blocked" in text else "env-блокер"
            suspects.append((name, why))
    elif summary != "CaughtMutant":
        suspects.append((name, summary))

print(f"# подозрительных: {len(suspects)}")
for name, why in suspects:
    print(name)
print(f"# причины: " + ", ".join(sorted({w for _, w in suspects})), file=sys.stderr)
