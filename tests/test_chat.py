"""Console chat: route/MCP wiring, request validation, prompt assembly, and the
OpenAI-compatible completion call (mocked transport)."""

import json

import httpx
import pytest
from pydantic import ValidationError

from rag_api.dto import ChatRequest, ChatResponse, ChatTurn
from rag_api.routers import chat as chat_router
from rag_core.config import ChatSettings
from rag_core.generation import ChatLLMError, chat_completion
from rag_core.schemas import SearchHit


# ---------------------------------------------------------------------------
# Wiring
# ---------------------------------------------------------------------------


def test_chat_route_registered():
    paths = {route.path for route in chat_router.router.routes}
    assert paths == {"/chat"}
    methods = {m for route in chat_router.router.routes for m in route.methods}
    assert methods == {"POST"}


def test_chat_not_exposed_over_mcp():
    """MCP stays search-only even with the chat router mounted."""
    from fastapi import FastAPI
    from fastapi_mcp import FastApiMCP

    from rag_api.routers import chat, search

    app = FastAPI()
    app.include_router(search.router)
    app.include_router(chat.router)
    mcp = FastApiMCP(app, include_operations=["search_documents", "list_searchable_collections"])
    assert {t.name for t in mcp.tools} == {"search_documents", "list_searchable_collections"}


# ---------------------------------------------------------------------------
# Request validation
# ---------------------------------------------------------------------------


def test_chat_turn_rejects_unknown_role():
    with pytest.raises(ValidationError):
        ChatTurn(role="system", content="hi")


def test_chat_request_requires_nonempty_query():
    with pytest.raises(ValidationError):
        ChatRequest(query="")


def test_chat_request_defaults():
    req = ChatRequest(query="what is the vacation policy?")
    assert req.history == []
    assert req.collection_ids == []


# ---------------------------------------------------------------------------
# Prompt assembly
# ---------------------------------------------------------------------------


def _hit(i: int, text: str) -> SearchHit:
    import uuid

    return SearchHit(
        chunk_id=uuid.uuid4(),
        document_id=uuid.uuid4(),
        collection_id=uuid.uuid4(),
        score=0.5,
        text=text,
        title=f"doc{i}.md",
    )


def test_context_block_numbers_and_labels_snippets():
    block = chat_router._context_block([_hit(1, "alpha"), _hit(2, "beta")])
    assert "[1] doc1.md" in block
    assert "alpha" in block
    assert "[2] doc2.md" in block
    assert "beta" in block


def test_response_carries_answer_hits_and_scope():
    import uuid

    cid = uuid.uuid4()
    resp = ChatResponse(answer="hello", hits=[_hit(1, "x")], searched_collection_ids=[cid])
    assert resp.answer == "hello"
    assert resp.searched_collection_ids == [cid]
    assert len(resp.hits) == 1


# ---------------------------------------------------------------------------
# Completion call (httpx.MockTransport)
# ---------------------------------------------------------------------------


def _patch_async_client(monkeypatch, handler):
    transport = httpx.MockTransport(handler)
    real = httpx.AsyncClient

    def patched(**kwargs):
        kwargs["transport"] = transport
        return real(**kwargs)

    monkeypatch.setattr(httpx, "AsyncClient", patched)


async def test_chat_completion_request_shape_and_key(monkeypatch):
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["url"] = str(request.url)
        seen["auth"] = request.headers.get("authorization")
        seen["payload"] = json.loads(request.content)
        return httpx.Response(200, json={"choices": [{"message": {"content": " The answer. "}}]})

    _patch_async_client(monkeypatch, handler)
    monkeypatch.setenv("OLLAMA_API_KEY", "secret")

    cfg = ChatSettings(
        enabled=True,
        base_url="http://localhost:11434/v1",
        model="llama3.1:8b",
        api_key_env="OLLAMA_API_KEY",
    )
    out = await chat_completion(cfg, [{"role": "user", "content": "hi"}])

    assert out == "The answer."  # stripped
    assert seen["url"].endswith("/chat/completions")
    assert seen["auth"] == "Bearer secret"
    assert seen["payload"]["model"] == "llama3.1:8b"
    assert seen["payload"]["stream"] is False


async def test_chat_completion_omits_auth_when_key_unset(monkeypatch):
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.headers.get("authorization") is None
        return httpx.Response(200, json={"choices": [{"message": {"content": "ok"}}]})

    _patch_async_client(monkeypatch, handler)
    monkeypatch.delenv("OLLAMA_API_KEY", raising=False)

    cfg = ChatSettings(enabled=True, api_key_env="OLLAMA_API_KEY")
    assert await chat_completion(cfg, [{"role": "user", "content": "hi"}]) == "ok"


async def test_chat_completion_raises_on_http_error(monkeypatch):
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(400, json={"error": "bad"})

    _patch_async_client(monkeypatch, handler)
    cfg = ChatSettings(enabled=True)
    with pytest.raises(ChatLLMError):
        await chat_completion(cfg, [{"role": "user", "content": "hi"}])


async def test_chat_completion_raises_on_empty_content(monkeypatch):
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json={"choices": [{"message": {"content": ""}}]})

    _patch_async_client(monkeypatch, handler)
    cfg = ChatSettings(enabled=True)
    with pytest.raises(ChatLLMError):
        await chat_completion(cfg, [{"role": "user", "content": "hi"}])
