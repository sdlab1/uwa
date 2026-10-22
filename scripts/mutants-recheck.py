#!/usr/bin/env python3
"""Перепроверка подозрительных вердиктов мутационного аудита (load-флак, §9.29).

Зачем: вердикт "Caught" вида «мутация env-блокер» — это ШУМ (внутренний load-гейт devh
заблокировал mutate-гейт внутри integration-теста, тест упал, cargo-mutants записал
мутанта как пойманного). Настоящий вердикт получается при HARNESS_MUTATE_MAX_LOAD=off.

Как: по списку мутантов (подозрительные + непойманные) собираем СПИСОК ФАЙЛОВ и
перезапускаем cargo-mutants по этим файлам (штатный -f, несколько файлов за раз)
с выключенным load-гейтом; затем сравниваем вердикты по ИМЕНИ мутанта.

    scripts/mutants-recheck.py --run-dir /tmp/accept --suspect-file suspects.txt
    scripts/mutants-recheck.py --run-dir /tmp/accept --suspect-file suspects.txt --no-run

--no-run: только показать, какие файлы и сколько мутантов перезапускать (без cargo-mutants).
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

VERDICTS = {
    "CaughtMutant": "caught",
    "MissedMutant": "missed",
    "Timeout": "timeout",
    "Unviable": "unviable",
}


def load_outcomes(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8", errors="replace"))


def mutant_rows(data: dict) -> list[dict]:
    rows = []
    for item in data.get("outcomes", []):
        mutant = (item.get("scenario") or {}).get("Mutant") or {}
        rows.append(
            {
                "name": mutant.get("name", "?"),
                "file": mutant.get("file", "?"),
                "summary": item.get("summary", "Unknown"),
                "verdict": VERDICTS.get(item.get("summary", ""), item.get("summary", "?")),
            }
        )
    return rows


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--run-dir", required=True, help="каталог прогона (там mutants.out/outcomes.json)")
    ap.add_argument("--suspect-file", required=True, help="паттерны имён мутантов, по строке на паттерн")
    ap.add_argument("--no-run", action="store_true")
    ap.add_argument("--out-json", default=None)
    ap.add_argument("--files", default=None, help="перезапустить только эти файлы (через запятую)")
    ap.add_argument("--profile", default="new", choices=["new", "old"])
    ap.add_argument("--orig", default=None, help="база для сверки (исходный outcomes.json); по умолчанию snapshots.orig.json")
    args = ap.parse_args()

    run_dir = Path(args.run_dir).resolve()
    # База (исходные вердикты) — отдельный файл: outcomes.json перезаписывается
    # НА ЛЕТУ текущим прогоном, и подстановка «прочитанного» ломает выбор скоупа.
    orig_path = Path(args.orig) if args.orig else (run_dir / "mutants.out" / "outcomes.orig.json")
    if not orig_path.is_file():
        print(f"нет базы для сверки: {orig_path}")
        return 2
    rows = mutant_rows(load_outcomes(orig_path))
    patterns = [
        line.strip()
        for line in Path(args.suspect_file).read_text(encoding="utf-8", errors="replace").splitlines()
        if line.strip() and not line.startswith("#")
    ]
    picked = [r for r in rows if any(p in r["name"] for p in patterns)]
    files = sorted({r["file"] for r in picked})
    if args.files:
        want = {f.strip() for f in args.files.split(",") if f.strip()}
        files = [f for f in files if f in want]
    print(f"мутантов всего в прогоне: {len(rows)}; подходящих под паттерны: {len(picked)}")
    print(f"файлов к перезапуску: {len(files)}")
    for f in files:
        n = sum(1 for r in picked if r["file"] == f)
        print(f"  {f}  ({n} подозрительных)")
    if args.no_run:
        return 0

    orig = orig_path
    cmd = [
        "cargo-mutants", "mutants", "--in-place", "--test-tool", "nextest",
        "--baseline=skip", "--timeout-multiplier", "2", "-p", "devh",
        "--config", "devharness/cargo-mutants.toml",
    ]
    for f in files:
        cmd += ["-f", f]
    env = dict(os.environ)
    env["HARNESS_MUTATE_MAX_LOAD"] = "off"  # снимаем load-флак (§9.29)
    if args.profile == "new":
        env["CARGO_PROFILE_TEST_INCREMENTAL"] = "true"
        env["CARGO_PROFILE_TEST_OPT_LEVEL"] = "0"
    print("запуск: " + " ".join(cmd))
    rc = subprocess.call(cmd, cwd=run_dir, env=env)
    if rc != 0:
        print(f"прогон вернул rc={rc}")
        return rc

    new_rows = mutant_rows(load_outcomes(orig))
    new_by_name = {r["name"]: r for r in new_rows}
    diffs = []
    for r in picked:
        n = new_by_name.get(r["name"])
        if n is None:
            diffs.append((r["name"], r["verdict"], "<нет в перепрогоне>"))
        elif n["verdict"] != r["verdict"]:
            diffs.append((r["name"], r["verdict"], n["verdict"]))
    print(f"\nсравнено мутантов: {len(picked)}; расхождений вердиктов: {len(diffs)}")
    for name, old, new in diffs[:60]:
        print(f"  {old:8s} -> {new:8s}  {name}")
    if args.out_json:
        Path(args.out_json).write_text(
            json.dumps(
                {
                    "compared": len(picked),
                    "diffs": [{"name": n, "old": o, "new": v} for n, o, v in diffs],
                },
                ensure_ascii=False,
                indent=2,
            ),
            encoding="utf-8",
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
