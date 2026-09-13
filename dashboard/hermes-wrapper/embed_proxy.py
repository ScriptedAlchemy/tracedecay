"""Same-origin Hermes embed path helpers for the dashboard iframe proxy.

The iframe must load on the Hermes origin. Loopback dashboard URLs resolve on
the browser's machine (or are mixed-content blocked) when Hermes is reached
from another host or over HTTPS.
"""

from __future__ import annotations

import re

EMBED_MOUNT = "/api/plugins/tracedecay/embed"
DASHBOARD_EMBED_PATH = f"{EMBED_MOUNT}/"

_ATTR_URL_RE = re.compile(
    r"""(?P<attr>\b(?:src|href))=(?P<quote>['"])/(?P<path>(?!/))""",
    re.IGNORECASE,
)


def embed_upstream_path(subpath: str) -> str:
    """Map an embed subpath onto the upstream dashboard path."""
    tail = subpath.strip("/")
    return f"/{tail}" if tail else "/"


def is_html_content_type(content_type: str | None) -> bool:
    if content_type is None:
        return False
    media = content_type.split(";", 1)[0].strip().lower()
    return media in {"text/html", "application/xhtml+xml"}


def is_event_stream(upstream_path: str, accept: str) -> bool:
    if "text/event-stream" in accept.lower():
        return True
    return upstream_path.rstrip("/") == "/api/events"


def dashboard_bridge_script() -> str:
    """Rewrite `/api` fetches and EventSource onto the Hermes embed prefix."""
    return (
        "<script>"
        "(function(){"
        f"var p={EMBED_MOUNT!r};"
        "function rw(u){"
        "if(typeof u!=='string')return u;"
        "if(u.indexOf('/api/')===0)return p+u;"
        "return u;"
        "}"
        "var f=window.fetch;"
        "window.fetch=function(i,n){"
        "if(typeof i==='string')i=rw(i);"
        "else if(typeof Request!=='undefined'&&i instanceof Request)"
        "i=new Request(rw(i.url),i);"
        "return f.call(this,i,n);"
        "};"
        "var ES=window.EventSource;"
        "function P(u,c){return new ES(rw(u),c)}"
        "P.prototype=ES.prototype;"
        "P.CONNECTING=ES.CONNECTING;P.OPEN=ES.OPEN;P.CLOSED=ES.CLOSED;"
        "window.EventSource=P;"
        "window.__webpack_public_path__=p+'/';"
        "})();"
        "</script>"
    )


def rewrite_dashboard_html(html: str) -> str:
    """Prefix root-absolute assets and inject the same-origin API bridge."""
    rewritten = _ATTR_URL_RE.sub(
        lambda match: (
            f"{match.group('attr')}={match.group('quote')}"
            f"{EMBED_MOUNT}/{match.group('path')}"
        ),
        html,
    )
    script = dashboard_bridge_script()
    lower = rewritten.lower()
    idx = lower.find("<head>")
    if idx == -1:
        return script + rewritten
    insert_at = idx + len("<head>")
    return rewritten[:insert_at] + script + rewritten[insert_at:]
