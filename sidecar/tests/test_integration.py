"""Integration tests. Requires `pip install nodriver`. Uses headless Chrome."""
import asyncio
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent))

from uwa_nodriver_sidecar import Sidecar


@pytest.mark.integration
@pytest.mark.asyncio
async def test_initialize_and_open_tab():
    sidecar = Sidecar()
    try:
        await sidecar.initialize({"headless": True, "extra_args": []})
        assert sidecar.browser is not None
        r = await sidecar.tab_open({"url": "data:text/html,<h1>hi</h1>"})
        tab_id = r["tab"]
        url = await sidecar.current_url({"tab": tab_id})
        assert "data:text/html" in url["url"]
        html = await sidecar.get_content({"tab": tab_id})
        assert "<h1>hi</h1>" in html["html"]
    finally:
        await sidecar.shutdown()


@pytest.mark.integration
@pytest.mark.asyncio
async def test_eval_and_stealth_webdriver():
    sidecar = Sidecar()
    try:
        await sidecar.initialize({"headless": True, "extra_args": []})
        r = await sidecar.tab_open({"url": "data:text/html,<p>x</p>"})
        tab_id = r["tab"]
        v = await sidecar.eval({"tab": tab_id, "js": "navigator.webdriver"})
        # Under nodriver, navigator.webdriver must be undefined/false.
        assert v["value"] in (None, False), f"got {v['value']!r}"
    finally:
        await sidecar.shutdown()


@pytest.mark.integration
@pytest.mark.asyncio
async def test_network_event_forwarded():
    sidecar = Sidecar()
    received = []

    # Intercept send_event.
    orig = sidecar.send_event

    async def spy(name, data):
        received.append((name, data))
        await orig(name, data)

    sidecar.send_event = spy
    try:
        await sidecar.initialize({"headless": True, "extra_args": []})
        r = await sidecar.tab_open({"url": "data:text/html,<p>y</p>"})
        tab_id = r["tab"]
        await sidecar.network_enable({"tab": tab_id})
        # Trigger a navigation (produces network events).
        await sidecar.navigate({"tab": tab_id, "url": "data:text/html,<p>z</p>"})
        await asyncio.sleep(1.0)
        # Should have at least one network.response or network.finished.
        names = [n for (n, _) in received]
        assert any("network." in n for n in names), f"got events: {names}"
    finally:
        await sidecar.shutdown()
