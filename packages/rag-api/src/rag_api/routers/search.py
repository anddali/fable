from __future__ import annotations

import re
import time
import uuid

from fastapi import APIRouter, HTTPException, status
from fastapi.responses import Response
from sqlalchemy import func, select
from sqlalchemy.orm import selectinload

from rag_api.auth import CurrentPrincipal
from rag_api.deps import BlobStoreDep, DbSession, SettingsDep
from rag_api.dto import (
    ChunkListOut,
    ChunkOut,
    SearchableCollection,
    SearchRequest,
    SearchResponse,
)
from rag_core.db.models import Chunk, Collection, Document, QueryLog
from rag_core.rbac import effective_collection_role
from rag_core.retrieval.service import search as run_search
from rag_core.schemas import Principal

router = APIRouter(tags=["search"])

# {collection_id}/{document_id}/figures/{hash16}-{n}.png — exactly the keys
# written by the indexer's vision step. Anything else (e.g. raw document
# payload keys) is rejected so this endpoint can only ever serve figures.
_FIGURE_KEY = re.compile(
    r"^(?P<collection_id>[0-9a-f-]{36})/[0-9a-f-]{36}/figures/[0-9a-f]{1,16}-\d+\.png$"
)


def parse_figure_key(key: str) -> uuid.UUID | None:
    """Collection id from a well-formed figure blob key, else None."""
    match = _FIGURE_KEY.match(key)
    if not match:
        return None
    try:
        return uuid.UUID(match.group("collection_id"))
    except ValueError:
        return None


async def _readable_collections(session, principal: Principal) -> dict[uuid.UUID, Collection]:
    """Collections the caller may read. API keys are further restricted to the
    intersection with their scopes (so a key never outlives its owner's direct
    access, and never exceeds its declared scope)."""
    result = await session.execute(select(Collection).options(selectinload(Collection.principals)))
    readable = {
        c.id: c
        for c in result.scalars()
        if principal.is_admin
        or effective_collection_role(principal, c.principals, public=c.visibility == "public")
    }
    if principal.key_scopes is not None:
        readable = {cid: c for cid, c in readable.items() if str(cid) in principal.key_scopes}
    return readable


@router.get("/search/collections", operation_id="list_searchable_collections")
async def list_searchable_collections(
    session: DbSession,
    principal: CurrentPrincipal,
) -> list[SearchableCollection]:
    """List the document collections the caller can search.

    Returns each collection's id, name and description. Pass ids to the
    search tool's ``collection_ids`` to restrict a search to specific
    collections.
    """
    readable = await _readable_collections(session, principal)
    return [
        SearchableCollection.model_validate(c)
        for c in sorted(readable.values(), key=lambda c: c.name)
    ]


@router.post("/search", operation_id="search_documents")
async def search(
    body: SearchRequest,
    session: DbSession,
    settings: SettingsDep,
    principal: CurrentPrincipal,
) -> SearchResponse:
    """Semantic search over the indexed document collections.

    Returns the most relevant document chunks for the query, with source
    metadata. Omit ``collection_ids`` to search every collection the caller
    can read.

    Each hit is a short *excerpt*, not the whole document — it is a sample for
    locating relevant material. Do NOT summarize, compare, or draw conclusions
    about an entire document from search hits alone; they are a small, non-
    contiguous slice. When the task needs more than the excerpt (summarizing a
    document, listing everything it says about X, reading the passage around a
    hit), call ``get_document_chunks`` with the hit's ``document_id`` to read
    the actual content.

    Figures (diagrams, charts) are already described in the chunk text by the
    indexer's vision step, so a figure's meaning is present in the results you
    get — you do not need the image itself to reason about it. The raw image is
    not available as a tool; it can only be fetched over REST at
    ``GET /search/figures?key=<metadata.figures[].key>`` (same Authorization
    header), for display in a UI.
    """
    # The retrieval query is hard-filtered to exactly the readable set.
    readable = await _readable_collections(session, principal)

    if body.collection_ids:
        denied = [cid for cid in body.collection_ids if cid not in readable]
        if denied:
            # Same shape as "doesn't exist" — don't leak collection existence.
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
        top_k=body.top_k,
        mode=body.mode,
    )
    latency_ms = int((time.perf_counter() - started) * 1000)

    session.add(
        QueryLog(
            principal_oid=principal.oid,
            principal_name=principal.name,
            auth_method=principal.auth_method,
            api_key_id=uuid.UUID(principal.api_key_id) if principal.api_key_id else None,
            query=body.query[:4000],
            mode=body.mode,
            top_k=body.top_k,
            collection_ids=[str(cid) for cid in target_ids],
            hit_count=len(hits),
            latency_ms=latency_ms,
        )
    )
    await session.commit()

    return SearchResponse(hits=hits, searched_collection_ids=target_ids)


@router.get("/documents/{document_id}/chunks", operation_id="get_document_chunks")
async def get_document_chunks(
    document_id: uuid.UUID,
    session: DbSession,
    principal: CurrentPrincipal,
    from_ordinal: int = 0,
    limit: int = 20,
) -> ChunkListOut:
    """Read consecutive chunks of a single document, ordered by position.

    Use this to read a document's real content instead of relying on
    ``search_documents`` excerpts. ``total`` is the document's full chunk count;
    the response returns chunks ``from_ordinal .. from_ordinal + limit - 1``.

    Two workflows:

    - Read the WHOLE document (summarize, analyze, list everything it says):
      start at ``from_ordinal=0`` and page forward, advancing ``from_ordinal``
      by the number of chunks returned each call, until you have collected all
      ``total`` chunks. Do not stop after one page — a summary built from a
      partial read is wrong.
    - Read AROUND a search hit (get the passage a match sits in): pass
      ``from_ordinal = hit.ordinal - 2`` for the chunks just before and after it.

    Access mirrors /search exactly: the document's collection must be readable
    by the caller (and inside an API key's scopes).
    """
    document = await session.get(Document, document_id)
    if document is None:
        raise HTTPException(status.HTTP_404_NOT_FOUND, "Document not found")
    readable = await _readable_collections(session, principal)
    if document.collection_id not in readable:
        # Same shape as "doesn't exist" — don't leak document existence.
        raise HTTPException(status.HTTP_404_NOT_FOUND, "Document not found")

    total = (
        await session.execute(
            select(func.count()).select_from(Chunk).where(Chunk.document_id == document_id)
        )
    ).scalar() or 0
    # Select columns explicitly so the embedding vector never leaves the DB.
    rows = await session.execute(
        select(
            Chunk.id,
            Chunk.ordinal,
            Chunk.token_count,
            Chunk.text,
            Chunk.chunk_metadata,
            Chunk.embedding.isnot(None).label("has_embedding"),
        )
        .where(Chunk.document_id == document_id, Chunk.ordinal >= max(from_ordinal, 0))
        .order_by(Chunk.ordinal)
        .limit(min(limit, 100))
    )
    items = [
        ChunkOut(
            id=r.id,
            ordinal=r.ordinal,
            token_count=r.token_count,
            text=r.text,
            chunk_metadata=r.chunk_metadata,
            has_embedding=bool(r.has_embedding),
        )
        for r in rows
    ]
    return ChunkListOut(items=items, total=total)


@router.get("/search/figures", operation_id="get_search_figure")
async def get_figure(
    key: str,
    session: DbSession,
    blob_store: BlobStoreDep,
    principal: CurrentPrincipal,
) -> Response:
    """Fetch a figure image referenced by a search hit's ``metadata.figures``.

    Access mirrors /search exactly: the figure's collection must be readable
    by the caller (and inside an API key's scopes).
    """
    collection_id = parse_figure_key(key)
    if collection_id is None:
        raise HTTPException(status.HTTP_404_NOT_FOUND, "Unknown figure")
    readable = await _readable_collections(session, principal)
    if collection_id not in readable:
        # Same shape as "doesn't exist" — don't leak collection existence.
        raise HTTPException(status.HTTP_404_NOT_FOUND, "Unknown figure")
    try:
        data = blob_store.get(key)
    except FileNotFoundError:
        raise HTTPException(status.HTTP_404_NOT_FOUND, "Unknown figure") from None
    return Response(
        content=data,
        media_type="image/png",
        headers={"cache-control": "private, max-age=3600"},
    )
