"""Falsifiable tests for the Hermes same-origin dashboard embed proxy."""

from __future__ import annotations

import json
import unittest
from pathlib import Path

from embed_proxy import (
    DASHBOARD_EMBED_PATH,
    EMBED_MOUNT,
    dashboard_bridge_script,
    embed_upstream_path,
    is_event_stream,
    is_html_content_type,
    rewrite_dashboard_html,
)


class EmbedPathTests(unittest.TestCase):
    def test_iframe_url_is_the_hermes_mount_not_loopback(self) -> None:
        self.assertEqual(DASHBOARD_EMBED_PATH, "/api/plugins/tracedecay/embed/")
        self.assertFalse(DASHBOARD_EMBED_PATH.startswith("http://"))
        self.assertNotIn("127.0.0.1", DASHBOARD_EMBED_PATH)
        self.assertNotIn("localhost", DASHBOARD_EMBED_PATH)

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

    def test_bridge_rewrites_api_fetch_and_eventsource(self) -> None:
        script = dashboard_bridge_script()
        self.assertIn(EMBED_MOUNT, script)
        self.assertIn("window.fetch", script)
        self.assertIn("window.EventSource", script)
        self.assertIn("/api/", script)
        self.assertIn("__webpack_public_path__", script)


class PluginApiContractTests(unittest.TestCase):
    def test_dashboard_url_handler_returns_the_embed_path(self) -> None:
        source = Path(__file__).with_name("plugin_api.py").read_text()
        self.assertIn('return JSONResponse({"url": DASHBOARD_EMBED_PATH})', source)
        self.assertNotIn('{"url": f"{base}/"}', source)
        self.assertIn('@router.api_route("/embed"', source)
        self.assertIn('@router.api_route("/embed/{path:path}"', source)

    def test_get_dashboard_url_never_returns_loopback(self) -> None:
        try:
            import plugin_api
        except ImportError:
            self.skipTest("fastapi is not installed in this environment")
        plugin_api._upstream_base = lambda: "http://127.0.0.1:59999"  # type: ignore[method-assign]
        response = plugin_api.get_dashboard_url()
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
