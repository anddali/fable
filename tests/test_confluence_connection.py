"""Global Confluence connection: settings resolution, URL parsing, back-compat."""

import base64

import httpx
import pytest

from rag_connector_confluence.connector import ConfluenceConnector
from rag_core.config import ConfluenceConnection, ConfluenceSettings, Settings


def test_default_connection_from_flat_settings():
    s = ConfluenceSettings(base_url="https://org.atlassian.net/wiki", user_email="svc@org.com")
    conn = s.connection("default")
    assert conn.configured
    assert conn.base_url == "https://org.atlassian.net/wiki"
    assert conn.api_token_env == "CONFLUENCE_API_TOKEN"


def test_unconfigured_default_is_not_configured():
    assert ConfluenceSettings().connection("default").configured is False


def test_named_connections_door_open():
    s = ConfluenceSettings(
        base_url="https://main.atlassian.net/wiki",
        user_email="main@org.com",
        connections={
            "hr": ConfluenceConnection(
                base_url="https://hr.atlassian.net/wiki", user_email="hr@org.com"
            )
        },
    )
    assert s.connection("hr").base_url == "https://hr.atlassian.net/wiki"
    assert s.connection("default").base_url == "https://main.atlassian.net/wiki"
    with pytest.raises(KeyError):
        s.connection("missing")


def test_settings_parse_confluence_env(monkeypatch):
    monkeypatch.setenv("RAG_CONFLUENCE__BASE_URL", "https://env.atlassian.net/wiki")
    monkeypatch.setenv("RAG_CONFLUENCE__USER_EMAIL", "env@org.com")
    monkeypatch.setenv("RAG_CONFLUENCE__API_TOKEN_ENV", "MY_TOKEN")
    s = Settings()
    conn = s.confluence.connection("default")
    assert conn.base_url == "https://env.atlassian.net/wiki"
    assert conn.api_token_env == "MY_TOKEN"


@pytest.mark.parametrize(
    "url,expected",
    [
        (
            "https://org.atlassian.net/wiki/spaces/ENG/pages/12345/Title",
            {"kind": "page", "id": "12345"},
        ),
        ("https://org.atlassian.net/wiki/spaces/ENG/overview", {"kind": "space", "id": "ENG"}),
        ("https://org.atlassian.net/wiki/spaces/ENG", {"kind": "space", "id": "ENG"}),
        ("https://org.atlassian.net/wiki/spaces/ENG/folder/999", {"kind": "folder", "id": "999"}),
    ],
)
def test_parse_target_url(url, expected):
    s = Settings()
    assert ConfluenceConnector.parse_target_url(url, s) == {"targets": [expected]}


def test_parse_target_url_unrecognized():
    assert ConfluenceConnector.parse_target_url("https://example.com/foo", Settings()) is None


def test_connector_reports_url_support_and_readiness():
    # Settings(confluence=...) is explicit, not Settings() — the latter reads
    # the real .env, which may legitimately have Confluence configured locally.
    assert ConfluenceConnector.supports_url_targets is True
    unconfigured = Settings(confluence=ConfluenceSettings())
    assert ConfluenceConnector.ready(unconfigured) is False
    configured = Settings(
        confluence=ConfluenceSettings(base_url="https://x.atlassian.net/wiki", user_email="a@b.c")
    )
    assert ConfluenceConnector.ready(configured) is True


def test_legacy_inline_config_still_resolves():
    """Sources created before the global connection keep working (no migration)."""
    global_settings = Settings(
        confluence=ConfluenceSettings(base_url="https://new.atlassian.net/wiki", user_email="n@o")
    )
    legacy = ConfluenceConnector.from_config(
        {
            "base_url": "https://old.atlassian.net/wiki",
            "user_email": "old@org.com",
            "api_token_env": "OLD_TOKEN",
            "targets": [{"kind": "space", "id": "AA"}],
        }
    )
    conn = legacy._connection(global_settings)
    assert conn.base_url == "https://old.atlassian.net/wiki"  # inline wins over global
    assert conn.api_token_env == "OLD_TOKEN"


def test_new_config_uses_global_connection():
    global_settings = Settings(
        confluence=ConfluenceSettings(base_url="https://new.atlassian.net/wiki", user_email="n@o")
    )
    new = ConfluenceConnector.from_config({"targets": [{"kind": "page", "id": "1"}]})
    assert new.config.connection == "default"
    assert new._connection(global_settings).base_url == "https://new.atlassian.net/wiki"


def test_sources_router_exposes_test_endpoint():
    from rag_api.routers import sources

    routes = {(r.path, m) for r in sources.router.routes for m in getattr(r, "methods", set())}
    assert ("/collections/{collection_id}/sources/test", "POST") in routes
    assert ("/collections/{collection_id}/sources", "POST") in routes


# --- Attachment image inlining ---------------------------------------------

_PNG_BYTES = b"\x89PNG fake image payload"
_STORAGE_BODY = (
    "<p>Intro text</p>"
    '<ac:image ac:align="center"><ri:attachment ri:filename="diagram.png" /></ac:image>'
    '<ac:image><ri:attachment ri:filename="missing.png" /></ac:image>'
    '<ac:image><ri:url ri:value="https://elsewhere.example/x.png" /></ac:image>'
    "<p>Outro text</p>"
)


def _confluence_transport() -> httpx.MockTransport:
    def handler(request: httpx.Request) -> httpx.Response:
        path = request.url.path
        if path == "/wiki/api/v2/pages/42":
            return httpx.Response(
                200,
                json={
                    "id": "42",
                    "title": "Page with images",
                    "spaceId": 7,
                    "version": {"number": 3, "createdAt": "2026-01-01T00:00:00Z"},
                    "body": {"storage": {"value": _STORAGE_BODY}},
                    "_links": {"webui": "/spaces/X/pages/42"},
                },
            )
        if path == "/wiki/api/v2/pages/42/attachments":
            return httpx.Response(
                200,
                json={
                    "results": [
                        {
                            "title": "diagram.png",
                            "mediaType": "image/png",
                            "downloadLink": "/download/attachments/42/diagram.png?api=v2",
                        }
                        # "missing.png" is referenced by the body but gone from
                        # the attachment list (deleted after the page was saved).
                    ]
                },
            )
        if path == "/wiki/download/attachments/42/diagram.png":
            return httpx.Response(200, content=_PNG_BYTES)
        return httpx.Response(404, json={})

    return httpx.MockTransport(handler)


@pytest.mark.asyncio
async def test_fetch_page_inlines_attachment_images_as_data_uris():
    connector = ConfluenceConnector.from_config({"targets": [{"kind": "page", "id": "42"}]})
    async with httpx.AsyncClient(
        transport=_confluence_transport(), base_url="https://x.atlassian.net/wiki"
    ) as client:
        doc = await connector._fetch_page(client, "https://x.atlassian.net/wiki", "42")

    assert doc is not None
    expected_b64 = base64.b64encode(_PNG_BYTES).decode("ascii")
    assert f'<img src="data:image/png;base64,{expected_b64}" alt="diagram.png">' in doc.text
    # Resolvable macro replaced; unresolvable ones left for the parser to drop.
    assert doc.text.count("<ac:image") == 2
    assert 'ri:filename="missing.png"' in doc.text
    assert "ri:url" in doc.text
    assert "Intro text" in doc.text and "Outro text" in doc.text


@pytest.mark.asyncio
async def test_fetch_page_skips_attachment_lookup_without_image_macros():
    """Pages without image macros must not pay the attachments round-trip."""
    calls: list[str] = []

    def handler(request: httpx.Request) -> httpx.Response:
        calls.append(request.url.path)
        return httpx.Response(
            200,
            json={
                "id": "7",
                "title": "Plain",
                "body": {"storage": {"value": "<p>No images here</p>"}},
                "version": {"number": 1},
                "_links": {},
            },
        )

    connector = ConfluenceConnector.from_config({"targets": [{"kind": "page", "id": "7"}]})
    async with httpx.AsyncClient(
        transport=httpx.MockTransport(handler), base_url="https://x.atlassian.net/wiki"
    ) as client:
        doc = await connector._fetch_page(client, "https://x.atlassian.net/wiki", "7")

    assert doc is not None and "No images here" in doc.text
    assert calls == ["/wiki/api/v2/pages/7"]


@pytest.mark.asyncio
async def test_iter_documents_raises_when_token_missing(monkeypatch):
    monkeypatch.delenv("CONFLUENCE_API_TOKEN", raising=False)
    settings = Settings(
        confluence=ConfluenceSettings(base_url="https://x.atlassian.net/wiki", user_email="a@b.c")
    )
    connector = ConfluenceConnector.from_config({"targets": [{"kind": "page", "id": "1"}]})

    class _Ctx:
        pass

    ctx = _Ctx()
    ctx.settings = settings
    with pytest.raises(RuntimeError, match="CONFLUENCE_API_TOKEN"):
        async for _ in connector.iter_documents(ctx):  # noqa: B007
            break
