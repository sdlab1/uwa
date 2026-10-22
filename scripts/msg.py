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
KEEP = int(os.environ.get("MSG_KEEP", "10"))
DEDUPE_SEC = int(os.environ.get("MSG_DEDUPE_SEC", "90"))
DEDUPE_LOG = Path(os.environ.get("MSG_DEDUPE_LOG", "/tmp/messages.notified"))
SEPARATOR = "=" * 80


def _dedupe_key(text: str) -> str:
    import hashlib

    head = "\n".join(text.splitlines()[:3])
    return hashlib.sha1(head.encode("utf-8", "replace")).hexdigest()[:16]


def _claim_notification(key: str) -> bool:
    """True — можно показывать; False — дубль (уже показывали недавно)."""
    now = time.time()
    entries: list[tuple[str, float]] = []
    if DEDUPE_LOG.is_file():
        for line in DEDUPE_LOG.read_text(encoding="utf-8", errors="replace").splitlines():
            parts = line.split()
            if len(parts) == 2:
                try:
                    entries.append((parts[0], float(parts[1])))
                except ValueError:
                    pass
    for k, ts in entries:
        if k == key and now - ts < DEDUPE_SEC:
            return False
    entries.append((key, now))
    DEDUPE_LOG.write_text(
        "\n".join(f"{k} {ts:.0f}" for k, ts in entries[-50:]) + "\n", encoding="utf-8"
    )
    return True


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
    key = _dedupe_key(text)
    if not _claim_notification(key):
        print("дубль уведомления (уже показано) — молчу", flush=True)
        return
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


def _split_blocks(data: bytes) -> tuple[bytes, list[tuple[int, int]]]:
    """-> (шапка, список блоков (start, end) в байтах)."""
    sep = (SEPARATOR + "\n").encode()
    first = data.find(sep)
    if first < 0:
        return data, []
    header = data[: first + len(sep)]
    blocks: list[tuple[int, int]] = []
    pos = first + len(sep)
    while True:
        nxt = data.find(sep, pos)
        if nxt < 0:
            break
        end = nxt + len(sep)
        blocks.append((pos, end))
        pos = end
    return header, blocks


def prune(args: argparse.Namespace) -> int:
    """Удалить сообщения, прочитанные ВСЕМИ сессиями; последние K — оставить."""
    if not CHANNEL.is_file():
        print("канала нет")
        return 0
    offsets: dict[Path, int] = {}
    for st in _session_states():
        try:
            offsets[st] = int(st.read_text(encoding="utf-8").strip() or 0)
        except (OSError, ValueError):
            continue
    if not offsets:
        print("нет ни одного курсора сессий — удалять нечего")
        return 0
    limit = min(offsets.values())
    data = CHANNEL.read_bytes()
    header, blocks = _split_blocks(data)
    if not blocks:
        print("блоков нет — не удаляю")
        return 0
    read_blocks = [b for b in blocks if b[1] <= limit]      # прочитаны всеми
    unread = [b for b in blocks if b[1] > limit]            # кто-то ещё не прочитал
    keep_read = read_blocks[-args.keep:] if args.keep > 0 else []
    removed = [b for b in read_blocks if b not in keep_read]
    if not removed:
        print("удалять нечего (прочитанное в окне последних K блоков)")
        return 0
    removed_bytes = sum(e - s for s, e in removed)
    kept_bytes = b"".join(data[s:e] for s, e in keep_read + unread)
    print(f"курсоры: " + ", ".join(f"{p.name}={v}" for p, v in offsets.items()))
    print(f"блоков: прочитано всеми={len(read_blocks)}, не прочитано={len(unread)}, "
          f"оставляю последних={len(keep_read)}, удаляю={len(removed)} ({removed_bytes} байт)")
    if args.dry_run:
        return 0
    CHANNEL.write_bytes(header + kept_bytes)
    first_removed_start = removed[0][0]
    for st, off in offsets.items():
        new_off = off if off <= first_removed_start else max(0, off - removed_bytes)
        st.write_text(str(new_off), encoding="utf-8")
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
                prune(argparse.Namespace(dry_run=False, keep=KEEP))
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
    p.add_argument("--keep", type=int, default=KEEP)
    p.set_defaults(func=prune)

    args = ap.parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
