"""API-layer tests that don't need Postgres (auth wiring, plugin introspection)."""

import pytest
from fastapi import FastAPI
from fastapi.testclient import TestClient

from rag_api.routers import health, plugins
from rag_core.config import get_settings


@pytest.fixture
def client(monkeypatch):
    monkeypatch.setenv("RAG_AUTH__MODE", "disabled")
    monkeypatch.setenv("RAG_ENVIRONMENT", "dev")
    get_settings.cache_clear()
    app = FastAPI()
    app.include_router(health.router)
    app.include_router(plugins.router)
    with TestClient(app) as test_client:
        yield test_client
    get_settings.cache_clear()


def test_liveness(client):
    assert client.get("/healthz").json() == {"status": "ok"}


def test_plugins_endpoint_lists_all_kinds(client):
    body = client.get("/plugins").json()
    assert set(body) == {"connector", "parser", "transformer", "chunker", "embedder", "vision"}
    connector_names = {p["name"] for p in body["connector"]}
    assert {"upload", "confluence", "sharepoint", "git", "slack"} <= connector_names


def test_mcp_exposes_exactly_the_search_tools():
    from fastapi_mcp import FastApiMCP

    from rag_api.routers import search

    app = FastAPI()
    app.include_router(search.router)
    mcp = FastApiMCP(
        app, include_operations=["search_documents", "list_searchable_collections"]
    )
    tools = {t.name: t for t in mcp.tools}
    assert set(tools) == {"search_documents", "list_searchable_collections"}
    assert "Semantic search" in tools["search_documents"].description
    assert "collections the caller can search" in tools["list_searchable_collections"].description


def test_mcp_exposes_document_chunk_paging():
    """The per-document chunk reader is offered alongside the search tools."""
    from fastapi_mcp import FastApiMCP

    from rag_api.routers import search

    app = FastAPI()
    app.include_router(search.router)
    mcp = FastApiMCP(
        app,
        include_operations=[
            "search_documents",
            "list_searchable_collections",
            "get_document_chunks",
        ],
    )
    tools = {t.name: t for t in mcp.tools}
    assert "get_document_chunks" in tools
    assert "consecutive chunks of a single document" in tools["get_document_chunks"].description


def test_document_chunks_endpoint_is_on_the_search_surface():
    """Chunk paging shares the API-key/MCP surface, not the SSO-only inspector."""
    from rag_api.routers import search

    route = next(
        r for r in search.router.routes if getattr(r, "operation_id", None) == "get_document_chunks"
    )
    assert route.path == "/documents/{document_id}/chunks"
    assert route.methods == {"GET"}


def test_auth_disabled_forbidden_in_prod(monkeypatch):
    monkeypatch.setenv("RAG_AUTH__MODE", "disabled")
    monkeypatch.setenv("RAG_ENVIRONMENT", "prod")
    get_settings.cache_clear()
    try:
        app = FastAPI()
        app.include_router(plugins.router)
        with (
            TestClient(app, raise_server_exceptions=True) as test_client,
            pytest.raises(RuntimeError, match="not permitted in prod"),
        ):
            test_client.get("/plugins")
    finally:
        get_settings.cache_clear()
