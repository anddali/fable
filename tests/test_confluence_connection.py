"""Global Confluence connection: settings resolution, URL parsing, back-compat."""

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
    assert ConfluenceConnector.supports_url_targets is True
    assert ConfluenceConnector.ready(Settings()) is False  # nothing configured
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
