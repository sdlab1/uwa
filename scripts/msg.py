#!/usr/bin/env python3
"""Канал связи сессий: /tmp/messages.txt (post / watch / tail).

    scripts/msg.py post --from "сессия-perf" --to "сессия-parallel" \
        --subject "Тема" --body "текст"            # дописать блок в канал
    scripts/msg.py post --from ... --body-file файл.txt
    scripts/msg.py watch                            # следить и ОПОВЕЩАТЬ о новых блоках
    scripts/msg.py watch --once                     # проверить один раз и выйти
    scripts/msg.py tail [N]                        # показать последние N блоков

Оповещение при новом сообщении: notify-send (десктоп) + звук + запись в
/tmp/messages.notify.log + печать в stdout. Состояние «прочитано до» —
/tmp/messages.state (смещение в байтах), поэтому повторные уведомления не
дублируются.
"""
from __future__ import annotations

import argparse
import os
import subprocess
import sys
import time
from pathlib import Path

CHANNEL = Path(os.environ.get("MSG_CHANNEL", "/tmp/messages.txt"))
SESSION = os.environ.get("MSG_SESSION", "default")
STATE = Path(os.environ.get("MSG_STATE", f"/tmp/messages.state.{SESSION}"))
NOTIFY_LOG = Path(os.environ.get("MSG_NOTIFY_LOG", "/tmp/messages.notify.log"))
NOTIFY_TIMEOUT_MS = int(os.environ.get("MSG_NOTIFY_TIMEOUT_MS", "20000"))
SEPARATOR = "=" * 80


def now() -> str:
    return time.strftime("%H:%M")


def post(args: argparse.Namespace) -> int:
    body = args.body
    if args.body_file:
        body = Path(args.body_file).read_text(encoding="utf-8", errors="replace").rstrip()
    body = (body or "").rstrip()
    block = (
        f"\n[{now()}] ОТ: {args.sender} — КОМУ: {args.to}\n"
        f"ТЕМА: {args.subject}\n"
        f"{body}\n{SEPARATOR}\n"
    )
    CHANNEL.parent.mkdir(parents=True, exist_ok=True)
    with CHANNEL.open("a", encoding="utf-8") as fh:
        fh.write(block)
    print(f"записано в {CHANNEL} ({len(block)} байт)")
    return 0


def _new_text() -> str:
    if not CHANNEL.is_file():
        return ""
    size = CHANNEL.stat().st_size
    offset = 0
    if STATE.is_file():
        try:
            offset = int(STATE.read_text(encoding="utf-8").strip() or 0)
        except ValueError:
            offset = 0
    if size < offset:  # файл пересоздан/обрезан
        offset = 0
    with CHANNEL.open("r", encoding="utf-8", errors="replace") as fh:
        fh.seek(offset)
        text = fh.read()
        STATE.write_text(str(fh.tell()), encoding="utf-8")
    return text


def _notify(text: str) -> None:
    first = next((ln for ln in text.splitlines() if ln.startswith("ТЕМА:")), "новое сообщение")
    sender = next((ln for ln in text.splitlines() if ln.startswith("[")), "")
    summary = f"{sender} — {first}"[:220]
    with NOTIFY_LOG.open("a", encoding="utf-8") as fh:
        fh.write(f"\n===== {time.strftime('%Y-%m-%d %H:%M:%S')} =====\n{text}\n")
    # -t: уведомление само исчезает (таймаут дема — /expire-timeout в xfce4-notifyd).
    # ВАЖНО: без -u critical! У critical-демон держит уведомление до ручного закрытия
    # (проверено 2026-10-22: три "вечных" окна xfce4-notifyd на экране).
    try:
        subprocess.run(
            ["notify-send", "-t", str(NOTIFY_TIMEOUT_MS),
             "Сессии: сообщение", summary],
            check=False, timeout=10,
        )
    except (OSError, subprocess.SubprocessError):
        pass
    try:
        subprocess.run(["paplay", "/usr/share/sounds/freedesktop/stereo/message.oga"], check=False, timeout=10)
    except (OSError, subprocess.SubprocessError):
        sys.stdout.write("\a")
    print("\n" + text, flush=True)


def _session_states() -> list[Path]:
    return sorted(Path("/tmp").glob("messages.state.*"))


def prune(args: argparse.Namespace) -> int:
    """Удалить сообщения, прочитанные ВСЕМИ сессиями (по их курсорам)."""
    if not CHANNEL.is_file():
        print("канала нет")
        return 0
    states = _session_states()
    offsets: dict[Path, int] = {}
    for st in states:
        try:
            offsets[st] = int(st.read_text(encoding="utf-8").strip() or 0)
        except (OSError, ValueError):
            continue
    if not offsets:
        print("нет ни одного курсора сессий — удалять нечего")
        return 0
    limit = min(offsets.values())
    data = CHANNEL.read_bytes()
    if limit <= 0:
        print("никто ещё ничего не прочитал — не удаляю")
        return 0
    if limit > len(data):
        print("курсор впереди файла (файл перезаписан) — не удаляю")
        return 0
    # режем только по границе блоков: последний SEPARATOR до limit
    cut = data.rfind((SEPARATOR + "\n").encode(), 0, limit)
    if cut < 0:
        print("граница блоков не найдена — не удаляю")
        return 0
    cut += len(SEPARATOR) + 1
    header_end = data.find((SEPARATOR + "\n").encode())
    header = data[: header_end + len(SEPARATOR) + 1] if header_end >= 0 else b""
    if cut <= len(header):
        print("весь канал уже прочитан всеми, но шапку не трогаю")
        return 0
    kept = header + data[cut:]
    removed = len(data) - len(kept)
    print(f"курсоры: " + ", ".join(f"{p.name}={v}" for p, v in offsets.items()))
    print(f"к удалению: {removed} байт, остаётся {len(kept)} байт")
    if args.dry_run:
        return 0
    CHANNEL.write_bytes(kept)
    # сдвигаем курсоры сессий на величину удалённого
    for st, off in offsets.items():
        st.write_text(str(max(0, off - removed)), encoding="utf-8")
    print("канал почищен, курсоры сдвинуты")
    return 0


def watch(args: argparse.Namespace) -> int:
    if args.once:
        text = _new_text()
        if text.strip():
            _notify(text)
        return 0
    print(f"слежу за {CHANNEL} (Ctrl+C — стоп)", flush=True)
    while True:
        text = _new_text()
        if text.strip():
            _notify(text)
            # авто-чистка: удаляем то, что прочитали ВСЕ сессии (курсоры)
            try:
                prune(argparse.Namespace(dry_run=False))
            except Exception:  # noqa: BLE001
                pass
        time.sleep(args.interval)


def tail(args: argparse.Namespace) -> int:
    if not CHANNEL.is_file():
        print("канала нет")
        return 0
    text = CHANNEL.read_text(encoding="utf-8", errors="replace")
    blocks = [b for b in text.split(SEPARATOR) if b.strip()]
    for block in blocks[-args.n :]:
        print(block.strip() + "\n" + SEPARATOR)
    return 0


def main() -> int:
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("post")
    p.add_argument("--from", dest="sender", required=True)
    p.add_argument("--to", required=True)
    p.add_argument("--subject", required=True)
    p.add_argument("--body", default="")
    p.add_argument("--body-file", default=None)
    p.set_defaults(func=post)

    p = sub.add_parser("watch")
    p.add_argument("--once", action="store_true")
    p.add_argument("--interval", type=float, default=5.0)
    p.set_defaults(func=watch)

    p = sub.add_parser("tail")
    p.add_argument("n", nargs="?", type=int, default=3)
    p.set_defaults(func=tail)

    p = sub.add_parser("prune")
    p.add_argument("--dry-run", action="store_true")
    p.set_defaults(func=prune)

    args = ap.parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
