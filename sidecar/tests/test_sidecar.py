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
