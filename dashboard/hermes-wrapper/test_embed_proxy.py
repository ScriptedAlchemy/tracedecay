"""Falsifiable tests for the Hermes same-origin dashboard embed proxy."""

from __future__ import annotations

import importlib.util
import json
import os
import shutil
import sys
import tempfile
import unittest
import unittest.mock
from pathlib import Path

from embed_proxy import (
    DASHBOARD_EMBED_PATH,
    EMBED_MOUNT,
    dashboard_bridge_script,
    dashboard_upstream,
    embed_upstream_path,
    is_event_stream,
    is_html_content_type,
    rewrite_dashboard_html,
)

WRAPPER_DIR = Path(__file__).resolve().parent


class DashboardUpstreamTests(unittest.TestCase):
    def test_launch_token_becomes_the_basic_credential(self) -> None:
        self.assertEqual(
            dashboard_upstream("http://127.0.0.1:7341/?token=ab12"),
            (
                "http://127.0.0.1:7341",
                {"Authorization": "Basic dHJhY2VkZWNheTphYjEy"},
            ),
        )

    def test_tokenless_url_carries_no_credential(self) -> None:
        self.assertEqual(
            dashboard_upstream("http://127.0.0.1:7341/"),
            ("http://127.0.0.1:7341", {}),
        )


class EmbedPathTests(unittest.TestCase):
    def test_upstream_path_strips_the_embed_prefix_only(self) -> None:
        self.assertEqual(embed_upstream_path(""), "/")
        self.assertEqual(embed_upstream_path("/"), "/")
        self.assertEqual(embed_upstream_path("delivery"), "/delivery")
        self.assertEqual(embed_upstream_path("api/events"), "/api/events")
        self.assertEqual(
            embed_upstream_path("api/events/delivery-ack"),
            "/api/events/delivery-ack",
        )


class HtmlRewriteTests(unittest.TestCase):
    def test_rewrites_root_assets_and_injects_the_api_bridge(self) -> None:
        html = (
            "<!doctype html><html><head>"
            '<link href="/index.css" rel="stylesheet">'
            '<script src="/index.js"></script>'
            "</head><body></body></html>"
        )
        rewritten = rewrite_dashboard_html(html)
        self.assertIn(f'<link href="{EMBED_MOUNT}/index.css"', rewritten)
        self.assertIn(f'<script src="{EMBED_MOUNT}/index.js"', rewritten)
        self.assertIn(dashboard_bridge_script(), rewritten)
        head = rewritten.lower().find("<head>")
        script_at = rewritten.find(dashboard_bridge_script())
        self.assertGreater(script_at, head)

    def test_leaves_protocol_relative_and_absolute_urls_alone(self) -> None:
        html = (
            "<head>"
            '<script src="//cdn.example/app.js"></script>'
            '<link href="https://example.test/app.css">'
            "</head>"
        )
        rewritten = rewrite_dashboard_html(html)
        self.assertIn('src="//cdn.example/app.js"', rewritten)
        self.assertIn('href="https://example.test/app.css"', rewritten)

@unittest.skipIf(
    importlib.util.find_spec("fastapi") is None,
    "fastapi is not installed in this environment",
)
class PluginApiContractTests(unittest.TestCase):
    def setUp(self) -> None:
        """Load a deployed copy the way Hermes does: by file path under its
        plugin module name, with the plugin directory off ``sys.path``."""
        deployed = tempfile.TemporaryDirectory()
        self.addCleanup(deployed.cleanup)
        self.dashboard_dir = Path(deployed.name)
        for name in ("plugin_api.py", "embed_proxy.py"):
            shutil.copy(WRAPPER_DIR / name, self.dashboard_dir / name)
        module_name = "hermes_dashboard_plugin_tracedecay"
        spec = importlib.util.spec_from_file_location(
            module_name, self.dashboard_dir / "plugin_api.py"
        )
        assert spec is not None and spec.loader is not None
        self.plugin = importlib.util.module_from_spec(spec)
        sys.modules[module_name] = self.plugin
        self.addCleanup(sys.modules.pop, module_name, None)
        spec.loader.exec_module(self.plugin)

    def test_embed_helpers_come_from_the_deployed_sibling(self) -> None:
        self.assertEqual(
            self.plugin.dashboard_upstream.__code__.co_filename,
            str(self.dashboard_dir / "embed_proxy.py"),
        )
        self.assertEqual(
            self.plugin.dashboard_upstream("http://127.0.0.1:7341/?token=ab12"),
            (
                "http://127.0.0.1:7341",
                {"Authorization": "Basic dHJhY2VkZWNheTphYjEy"},
            ),
        )

    def test_get_dashboard_url_never_returns_loopback(self) -> None:
        external = {"TRACEDECAY_DASHBOARD_URL": "http://127.0.0.1:59999/?token=t"}
        with unittest.mock.patch.dict(os.environ, external):
            response = self.plugin.get_dashboard_url()
        payload = json.loads(bytes(response.body).decode("utf-8"))
        self.assertEqual(payload["url"], DASHBOARD_EMBED_PATH)
        self.assertNotIn("127.0.0.1", payload["url"])


class ContentTypeTests(unittest.TestCase):
    def test_html_detection(self) -> None:
        self.assertTrue(is_html_content_type("text/html; charset=utf-8"))
        self.assertTrue(is_html_content_type("application/xhtml+xml"))
        self.assertFalse(is_html_content_type("application/json"))
        self.assertFalse(is_html_content_type(None))

    def test_event_stream_detection(self) -> None:
        self.assertTrue(is_event_stream("/api/events", "text/html"))
        self.assertTrue(is_event_stream("/other", "text/event-stream"))
        self.assertFalse(is_event_stream("/api/events/delivery-ack", "application/json"))


if __name__ == "__main__":
    unittest.main()
