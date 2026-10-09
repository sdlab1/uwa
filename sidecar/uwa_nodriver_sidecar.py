#!/usr/bin/env python3
"""
uwa nodriver sidecar.

Speaks line-delimited JSON-RPC over stdin/stdout with the Rust `uwa-bin`
daemon. Wraps nodriver's async API and forwards CDP network events.

Protocol (one JSON object per line):
  → {"id": 1, "method": "navigate", "params": {"tab": "...", "url": "..."}}
  ← {"id": 1, "result": {"ok": true}}
  ← {"event": "network.response", "data": {...}}   # notifications

Methods:
  initialize(cfg)         -> {version, chrome_path}
  shutdown()              -> {ok: true}
  tabs_list()             -> {tabs: [{id, url, title}]}
  tab_open(url)           -> {tab: "..."}
  tab_close(tab)          -> {ok: true}
  navigate(tab, url)      -> {ok: true}
  current_url(tab)        -> {url: "..."}
  eval(tab, js)           -> {value: <json>}
  eval_in_frame(tab, frame_id, js) -> {value}
  frames_list(tab)        -> {frames: [{frame_id, url}]}
  get_content(tab)        -> {html: "..."}
  click(tab, selector)    -> {ok: true}
  type_text(tab, selector, text) -> {ok: true}
  wait_for_selector(tab, selector, timeout_ms) -> {ok: true}
  screenshot(tab)         -> {base64: "..."}
  network_enable(tab)     -> {ok: true}
"""
from __future__ import annotations

import asyncio
import base64
import json
import sys
import traceback
from typing import Any

import nodriver as uc
from nodriver import cdp


class Sidecar:
    def __init__(self) -> None:
        self.browser: uc.Browser | None = None
        # tab_id → nodriver Tab
        self._tabs: dict[str, uc.Tab] = {}
        self._write_lock = asyncio.Lock()
        self._next_tab = 0

    # ---------- I/O ----------

    async def send(self, obj: dict[str, Any]) -> None:
        async with self._write_lock:
            sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n")
            sys.stdout.flush()

    async def send_result(self, req_id: Any, result: Any) -> None:
        await self.send({"id": req_id, "result": result})

    async def send_error(self, req_id: Any, code: int, message: str) -> None:
        await self.send({"id": req_id, "error": {"code": code, "message": message}})

    async def send_event(self, name: str, data: Any) -> None:
        await self.send({"event": name, "data": data})

    # ---------- lifecycle ----------

    async def initialize(self, params: dict[str, Any]) -> dict[str, Any]:
        extra = list(params.get("extra_args") or [])
        headless = bool(params.get("headless"))
        if headless:
            extra.append("--headless=new")

        config = uc.Config(
            headless=headless,
            user_data_dir=params.get("user_data_dir"),
            browser_executable_path=params.get("browser_path"),
            browser_args=extra,
        )
        self.browser = await uc.start(config=config)
        return {
            "version": "uwa-nodriver-sidecar/1",
            "chrome_path": str(self.browser.config.browser_executable_path or ""),
        }

    async def shutdown(self) -> dict[str, Any]:
        if self.browser is not None:
            self.browser.stop()
            self.browser = None
        return {"ok": True}

    # ---------- tabs ----------

    async def tabs_list(self) -> dict[str, Any]:
        tabs = []
        for tab_id, tab in self._tabs.items():
            try:
                url = await tab.evaluate("location.href") or ""
                title = await tab.evaluate("document.title") or ""
            except Exception:
                url, title = "", ""
            tabs.append({"id": tab_id, "url": url, "title": title})
        return {"tabs": tabs}

    async def tab_open(self, params: dict[str, Any]) -> dict[str, Any]:
        url = params["url"]
        tab = await self.browser.get(url, new_tab=True)
        self._next_tab += 1
        tab_id = f"tab_{self._next_tab}"
        self._tabs[tab_id] = tab
        self._install_tab_handlers(tab_id, tab)
        return {"tab": tab_id}

    async def tab_close(self, params: dict[str, Any]) -> dict[str, Any]:
        tab_id = params["tab"]
        tab = self._tabs.pop(tab_id, None)
        if tab is not None:
            await tab.close()
        return {"ok": True}

    # ---------- navigation / eval ----------

    async def navigate(self, params: dict[str, Any]) -> dict[str, Any]:
        tab = self._require_tab(params["tab"])
        await tab.get(params["url"])
        return {"ok": True}

    async def current_url(self, params: dict[str, Any]) -> dict[str, Any]:
        tab = self._require_tab(params["tab"])
        url = await tab.evaluate("location.href") or ""
        return {"url": url}

    async def eval(self, params: dict[str, Any]) -> dict[str, Any]:
        tab = self._require_tab(params["tab"])
        result = await tab.evaluate(params["js"], return_by_value=True)
        # nodriver returns a RemoteObject; unwrap to the plain value.
        if hasattr(result, "value"):
            value = result.value
        elif isinstance(result, tuple) and len(result) == 2:
            # (RemoteObject, ExceptionDetails|None)
            remote, exc = result
            if exc is not None:
                raise RuntimeError(f"eval error: {exc.text}")
            value = remote.value if hasattr(remote, "value") else remote
        else:
            value = result
        return {"value": value}

    async def eval_in_frame(self, params: dict[str, Any]) -> dict[str, Any]:
        """Evaluate JS in a specific frame via an isolated world."""
        tab = self._require_tab(params["tab"])
        frame_id = params["frame_id"]
        js = params["js"]

        # Create an isolated world for the frame, then evaluate in it.
        world = await tab.send(
            cdp.page.create_isolated_world(frame_id=frame_id, world_name="uwa-eval")
        )
        result = await tab.evaluate(
            js, await_promise=True, return_by_value=True
        )
        # Unwrap RemoteObject.
        if hasattr(result, "value"):
            value = result.value
        elif isinstance(result, tuple) and len(result) == 2:
            remote, exc = result
            if exc is not None:
                raise RuntimeError(f"eval_in_frame error: {exc.text}")
            value = remote.value if hasattr(remote, "value") else remote
        else:
            value = result
        return {"value": value}

    async def frames_list(self, params: dict[str, Any]) -> dict[str, Any]:
        tab = self._require_tab(params["tab"])
        tree = await tab.send(cdp.page.get_frame_tree())
        frames: list[dict[str, Any]] = []

        def walk(node: Any, parent: str | None = None) -> None:
            fid = str(node.frame.id_)
            frames.append({"frame_id": fid, "url": node.frame.url, "parent": parent})
            for child in node.child_frames or []:
                walk(child, fid)

        walk(tree.frame_tree)
        return {"frames": frames}

    # ---------- content ----------

    async def get_content(self, params: dict[str, Any]) -> dict[str, Any]:
        tab = self._require_tab(params["tab"])
        html = await tab.get_content()
        return {"html": html}

    async def click(self, params: dict[str, Any]) -> dict[str, Any]:
        tab = self._require_tab(params["tab"])
        sel = params["selector"]
        el = await tab.select(sel, timeout=5)
        if el is None:
            raise ValueError(f"click: element `{sel}` not found")
        await el.click()
        return {"ok": True}

    async def type_text(self, params: dict[str, Any]) -> dict[str, Any]:
        tab = self._require_tab(params["tab"])
        sel = params["selector"]
        text = params["text"]
        el = await tab.select(sel, timeout=5)
        if el is None:
            raise ValueError(f"type_text: element `{sel}` not found")
        await el.send_keys(text)
        return {"ok": True}

    async def wait_for_selector(self, params: dict[str, Any]) -> dict[str, Any]:
        tab = self._require_tab(params["tab"])
        sel = params["selector"]
        timeout_ms = int(params.get("timeout_ms") or 10_000)
        el = await tab.select(sel, timeout=timeout_ms / 1000)
        if el is None:
            raise TimeoutError(f"wait_for_selector: `{sel}` not found")
        return {"ok": True}

    async def screenshot(self, params: dict[str, Any]) -> dict[str, Any]:
        tab = self._require_tab(params["tab"])
        png = await tab.save_screenshot(format="png")
        return {"base64": base64.b64encode(png).decode()}

    # ---------- network ----------

    async def network_enable(self, params: dict[str, Any]) -> dict[str, Any]:
        tab = self._require_tab(params["tab"])
        await tab.send(cdp.network.enable())
        return {"ok": True}

    # ---------- handlers ----------

    def _install_tab_handlers(self, tab_id: str, tab: uc.Tab) -> None:
        """Attach CDP event handlers and forward to Rust.

        nodriver 0.50 uses `tab.add_handler(event_type, callback)` — NOT the
        `@tab.on(...)` decorator pattern (removed in newer versions).
        """

        async def on_response(ev: cdp.network.ResponseReceived) -> None:
            await self.send_event(
                "network.response",
                {
                    "tab": tab_id,
                    "request_id": str(ev.request_id),
                    "url": ev.response.url,
                    "mime": ev.response.mime_type or "",
                },
            )

        async def on_finished(ev: cdp.network.LoadingFinished) -> None:
            # Fetch body for matching responses.
            try:
                body, _ = await tab.send(
                    cdp.network.get_response_body(request_id=ev.request_id)
                )
            except Exception:
                body = ""
            await self.send_event(
                "network.finished",
                {
                    "tab": tab_id,
                    "request_id": str(ev.request_id),
                    "body": body or "",
                },
            )

        async def on_frame_attached(ev: cdp.page.FrameAttached) -> None:
            await self.send_event(
                "oopif.attached",
                {
                    "tab": tab_id,
                    "frame_id": str(ev.frame_id),
                    "parent_frame_id": str(ev.parent_frame_id)
                    if ev.parent_frame_id
                    else None,
                },
            )

        async def on_frame_detached(ev: cdp.page.FrameDetached) -> None:
            await self.send_event(
                "oopif.detached",
                {"tab": tab_id, "frame_id": str(ev.frame_id)},
            )

        tab.add_handler(cdp.network.ResponseReceived, on_response)
        tab.add_handler(cdp.network.LoadingFinished, on_finished)
        tab.add_handler(cdp.page.FrameAttached, on_frame_attached)
        tab.add_handler(cdp.page.FrameDetached, on_frame_detached)

    # ---------- helpers ----------

    def _require_tab(self, tab_id: str) -> uc.Tab:
        tab = self._tabs.get(tab_id)
        if tab is None:
            raise ValueError(f"unknown tab `{tab_id}`")
        return tab

    # ---------- dispatch ----------

    async def dispatch(self, method: str, params: dict[str, Any]) -> Any:
        fn = getattr(self, method, None)
        if fn is None or not callable(fn):
            raise ValueError(f"unknown method `{method}`")
        return await fn(params)

    # ---------- main loop ----------

    async def run(self) -> None:
        loop = asyncio.get_event_loop()
        while True:
            line = await loop.run_in_executor(None, sys.stdin.readline)
            if not line:
                break
            line = line.strip()
            if not line:
                continue
            try:
                req = json.loads(line)
            except json.JSONDecodeError as e:
                await self.send_error(None, -32700, f"parse error: {e}")
                continue

            req_id = req.get("id")
            method = req.get("method", "")
            params = req.get("params", {}) or {}
            try:
                result = await self.dispatch(method, params)
                await self.send_result(req_id, result)
            except Exception as e:
                await self.send_error(req_id, -32000, f"{type(e).__name__}: {e}")
                traceback.print_exc(file=sys.stderr)


async def main() -> None:
    sidecar = Sidecar()
    try:
        await sidecar.run()
    finally:
        try:
            await sidecar.shutdown()
        except Exception:
            pass


if __name__ == "__main__":
    uc.loop().run_until_complete(main())
