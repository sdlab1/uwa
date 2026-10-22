#!/usr/bin/env python3
"""Перепроверка подозрительных вердиктов мутационного аудита (load-флак, §9.29).

Собирает комбинированный diff из per-mutant диффов (mutants.out/diff/*.diff) для
указанных мутантов и гоняет cargo-mutants --in-diff с HARNESS_MUTATE_MAX_LOAD=off:
внутренний load-гейт devh выключен -> вердикты перестают зависеть от загрузки
машины (иначе «Caught» = «мутация env-блокер», т.е. шум).

    scripts/mutants-recheck.py --run-dir /tmp/accept \
        --suspect-file /tmp/accept/suspects.txt

suspects.txt — по одному имени мутанта в строке (как в outcomes.json: Mutant.name)
или по имени файла лога (devharness__src__core__coverage.rs_line_37_col_9.log).
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path


def mutant_names(run_dir: Path, patterns: list[str]) -> list[str]:
    outcomes = json.loads((run_dir / "mutants.out" / "outcomes.json").read_text(encoding="utf-8", errors="replace"))
    wanted: list[str] = []
    for item in outcomes.get("outcomes", []):
        mutant = item.get("scenario", {}).get("Mutant") or {}
        name = mutant.get("name", "")
        if any(pat in name for pat in patterns):
            wanted.append(name)
    return wanted


def diff_path_for(run_dir: Path, name: str) -> Path | None:
    log = (run_dir / "mutants.out" / "outcomes.json")
    data = json.loads(log.read_text(encoding="utf-8", errors="replace"))
    for item in data.get("outcomes", []):
        mutant = item.get("scenario", {}).get("Mutant") or {}
        if mutant.get("name") == name:
            rel = item.get("diff_path")
            if rel:
                return run_dir / "mutants.out" / rel
    return None


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--run-dir", required=True)
    ap.add_argument("--suspect-file", required=True)
    ap.add_argument("--out", default=None)
    args = ap.parse_args()

    run_dir = Path(args.run_dir).resolve()
    patterns = [
        line.strip()
        for line in Path(args.suspect_file).read_text(encoding="utf-8", errors="replace").splitlines()
        if line.strip() and not line.startswith("#")
    ]
    names = mutant_names(run_dir, patterns)
    print(f"подозрительных по паттернам: {len(patterns)} -> мутантов найдено: {len(names)}")

    combined: list[str] = []
    missing: list[str] = []
    for name in names:
        path = diff_path_for(run_dir, name)
        if path and path.is_file():
            combined.append(path.read_text(encoding="utf-8", errors="replace"))
        else:
            missing.append(name)
    if missing:
        print(f"без диффа: {len(missing)} (пропущены)")
        for name in missing[:10]:
            print(f"  {name}")

    out = Path(args.out) if args.out else run_dir / "recheck.diff"
    out.write_text("".join(combined), encoding="utf-8")
    print(f"комбинированный diff: {out} ({out.stat().st_size} байт)")

    env = dict(os.environ)
    env["HARNESS_MUTATE_MAX_LOAD"] = "off"  # снимаем load-флак (§9.29)
    cmd = [
        "cargo-mutants", "mutants", "--in-place", "--test-tool", "nextest",
        "--baseline=skip", "--timeout-multiplier", "2", "-p", "devh",
        "--config", "devharness/cargo-mutants.toml", "--in-diff", str(out),
    ]
    print("запуск: " + " ".join(cmd))
    return subprocess.call(cmd, cwd=run_dir, env=env)


if __name__ == "__main__":
    sys.exit(main())
