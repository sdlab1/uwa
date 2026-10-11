"""Unit tests for the nodriver sidecar. Run with: pytest sidecar/tests/ -m 'not integration'."""
from __future__ import annotations

import asyncio
import json
import sys
from pathlib import Path

import pytest

# Ensure sidecar/ is on sys.path so we can import the module.
sys.path.insert(0, str(Path(__file__).parent.parent))

from uwa_nodriver_sidecar import Sidecar


@pytest.fixture
def sidecar():
    return Sidecar()


@pytest.mark.asyncio
async def test_unknown_method_raises(sidecar):
    with pytest.raises(ValueError, match="unknown method"):
        await sidecar.dispatch("bogus", {})


@pytest.mark.asyncio
async def test_send_writes_json_line(sidecar, capsys):
    await sidecar.send({"hello": "world"})
    captured = capsys.readouterr()
    assert captured.out == '{"hello": "world"}\n'


@pytest.mark.asyncio
async def test_send_error_formats_correctly(sidecar, capsys):
    await sidecar.send_error(42, -32000, "something broke")
    captured = capsys.readouterr()
    parsed = json.loads(captured.out.strip())
    assert parsed["id"] == 42
    assert parsed["error"]["code"] == -32000
    assert parsed["error"]["message"] == "something broke"


@pytest.mark.asyncio
async def test_send_result_formats_correctly(sidecar, capsys):
    await sidecar.send_result(7, {"ok": True})
    captured = capsys.readouterr()
    parsed = json.loads(captured.out.strip())
    assert parsed["id"] == 7
    assert parsed["result"] == {"ok": True}


@pytest.mark.asyncio
async def test_send_event_formats_correctly(sidecar, capsys):
    await sidecar.send_event("network.response", {"url": "https://example.com"})
    captured = capsys.readouterr()
    parsed = json.loads(captured.out.strip())
    assert parsed["event"] == "network.response"
    assert parsed["data"]["url"] == "https://example.com"


@pytest.mark.asyncio
async def test_require_tab_raises_on_unknown(sidecar):
    with pytest.raises(ValueError, match="unknown tab"):
        sidecar._require_tab("tab_missing")


@pytest.mark.asyncio
async def test_sidecar_shutdown_without_browser_is_noop(sidecar):
    result = await sidecar.shutdown()
    assert result == {"ok": True}


@pytest.mark.asyncio
async def test_tabs_list_empty(sidecar):
    result = await sidecar.tabs_list()
    assert result == {"tabs": []}


@pytest.mark.asyncio
async def test_tab_close_unknown_tab_is_ok(sidecar):
    result = await sidecar.tab_close({"tab": "tab_nonexistent"})
    assert result == {"ok": True}


def test_sidecar_module_exports_methods():
    """Every method in the spec must exist on Sidecar."""
    expected = [
        "initialize", "shutdown", "tabs_list", "tab_open", "tab_close",
        "navigate", "current_url", "eval", "eval_in_frame", "frames_list",
        "get_content", "click", "type_text", "wait_for_selector",
        "screenshot", "network_enable",
    ]
    s = Sidecar()
    for method in expected:
        assert hasattr(s, method), f"missing method: {method}"
        assert callable(getattr(s, method)), f"not callable: {method}"


# ---- eval_in_frame regression -------------------------------------------
#
# The old implementation created an isolated world for the frame and then
# evaluated via `tab.evaluate`, which always runs in the top-level context —
# `frame_id` was silently ignored. The test asserts the evaluation happens
# in the world's context.
#
# nodriver cdp commands are generators: the command name is the code object
# name and the arguments live in the generator frame's locals.

def _cmd_name(cmd) -> str:
    return cmd.gi_frame.f_code.co_name


def _cmd_args(cmd) -> dict:
    return dict(cmd.gi_frame.f_locals)


class _World:
    def __init__(self, context_id):
        self.context_id = context_id


class _Remote:
    def __init__(self, value):
        self.value = value


class _EvalResponse:
    def __init__(self, result, exception_details=None):
        self.result = result
        self.exception_details = exception_details


class FakeTab:
    def __init__(self, world_context=42, value="frame-value"):
        self.calls = []
        self._world = _World(world_context)
        self._value = value

    async def send(self, cmd):
        self.calls.append(cmd)
        name = _cmd_name(cmd)
        if name == "create_isolated_world":
            return self._world
        if name == "evaluate":
            return _EvalResponse(_Remote(self._value))
        raise AssertionError(f"unexpected cdp call: {name}")


@pytest.mark.asyncio
async def test_eval_in_frame_uses_the_frames_world():
    s = Sidecar()
    fake = FakeTab(world_context=77, value="in-frame")
    s._tabs["tab_1"] = fake

    out = await s.eval_in_frame({"tab": "tab_1", "frame_id": "F9", "js": "1+1"})

    assert out == {"value": "in-frame"}
    names = [_cmd_name(c) for c in fake.calls]
    assert names == ["create_isolated_world", "evaluate"], names

    world_cmd, eval_cmd = fake.calls
    world_args = _cmd_args(world_cmd)
    assert world_args["frame_id"] == "F9"
    assert world_args["world_name"] == "uwa-eval"
    # The regression: evaluation must run in the isolated world's context.
    assert _cmd_args(eval_cmd)["context_id"] == 77
    assert _cmd_args(eval_cmd)["expression"] == "1+1"


@pytest.mark.asyncio
async def test_eval_in_frame_raises_on_exception_details():
    s = Sidecar()

    class FailingTab(FakeTab):
        async def send(self, cmd):
            self.calls.append(cmd)
            name = _cmd_name(cmd)
            if name == "create_isolated_world":
                return self._world
            if name == "evaluate":
                exc = type("Exc", (), {"text": "boom"})()
                return _EvalResponse(_Remote(None), exception_details=exc)
            raise AssertionError(name)

    s._tabs["tab_2"] = FailingTab()
    with pytest.raises(RuntimeError, match="boom"):
        await s.eval_in_frame({"tab": "tab_2", "frame_id": "F1", "js": "x"})
