"""Conversational answers grounded in retrieved chunks (console only).

A chat turn = retrieve the most relevant chunks for the question across the
collections the caller can read, then ask a chat LLM to answer using only that
context plus the last few turns of conversation. SSO-gated (API keys stay
search-only) and inert unless a chat model is configured via ``RAG_CHAT__*``.
"""

from __future__ import annotations

import time

from fastapi import APIRouter, HTTPException, status

from rag_api.auth import CurrentUser
from rag_api.deps import DbSession, SettingsDep
from rag_api.dto import ChatRequest, ChatResponse
from rag_api.routers.search import _readable_collections
from rag_core.db.models import QueryLog
from rag_core.generation import ChatLLMError, chat_completion
from rag_core.retrieval.service import search as run_search
from rag_core.schemas import SearchHit

router = APIRouter(tags=["chat"])

SYSTEM_PROMPT = (
    "You are a retrieval assistant for an internal document platform. Answer "
    "the user's question using ONLY the numbered context snippets provided with "
    "the question. Cite the snippets you rely on inline as [1], [2], and so on. "
    "If the snippets do not contain the answer, say you don't know instead of "
    "guessing. Keep answers concise and factual."
)


def _context_block(hits: list[SearchHit]) -> str:
    return "\n\n".join(
        f"[{i}] {h.title or h.uri or 'untitled'}\n{h.text}" for i, h in enumerate(hits, 1)
    )


@router.post("/chat", operation_id="chat_over_documents", include_in_schema=True)
async def chat(
    body: ChatRequest,
    session: DbSession,
    settings: SettingsDep,
    principal: CurrentUser,
) -> ChatResponse:
    """Answer a question grounded in the indexed collections, with short memory."""
    cfg = settings.chat
    if not cfg.enabled:
        raise HTTPException(status.HTTP_503_SERVICE_UNAVAILABLE, "Chat is not configured")

    # Same hard ACL filter as /search: never retrieve outside the readable set.
    readable = await _readable_collections(session, principal)
    if body.collection_ids:
        denied = [cid for cid in body.collection_ids if cid not in readable]
        if denied:
            raise HTTPException(status.HTTP_404_NOT_FOUND, f"Unknown collection(s): {denied}")
        target_ids = list(body.collection_ids)
    else:
        target_ids = list(readable)

    started = time.perf_counter()
    hits = await run_search(
        session,
        settings,
        query=body.query,
        collection_ids=target_ids,
        top_k=cfg.top_k,
        mode=cfg.mode,
    )

    # system instruction · last N turns of memory · question carrying its context.
    messages: list[dict] = [{"role": "system", "content": SYSTEM_PROMPT}]
    for turn in body.history[-(cfg.history_turns * 2) :]:
        messages.append({"role": turn.role, "content": turn.content})
    context = _context_block(hits) if hits else "(no snippets matched the query)"
    messages.append(
        {
            "role": "user",
            "content": f"Context snippets:\n\n{context}\n\n---\nQuestion: {body.query}",
        }
    )

    try:
        answer = await chat_completion(cfg, messages)
    except ChatLLMError as exc:
        raise HTTPException(status.HTTP_502_BAD_GATEWAY, f"Chat model error: {exc}") from exc

    latency_ms = int((time.perf_counter() - started) * 1000)
    session.add(
        QueryLog(
            principal_oid=principal.oid,
            principal_name=principal.name,
            auth_method=principal.auth_method,
            query=body.query[:4000],
            mode="chat",
            top_k=cfg.top_k,
            collection_ids=[str(cid) for cid in target_ids],
            hit_count=len(hits),
            latency_ms=latency_ms,
        )
    )
    await session.commit()

    return ChatResponse(answer=answer, hits=hits, searched_collection_ids=target_ids)
