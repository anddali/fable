"""Read-only index inspection: what documents a collection holds and the
chunks they produced. Viewer-gated; never exposed over MCP or to API keys."""

from __future__ import annotations

import uuid

from fastapi import APIRouter, HTTPException
from sqlalchemy import func, or_, select

from rag_api.deps import DbSession, collection_access
from rag_api.dto import (
    ChunkListOut,
    ChunkOut,
    DocumentDetailOut,
    DocumentListOut,
    DocumentWithChunksOut,
)
from rag_core.db.models import Chunk, Collection, Document
from rag_core.rbac import CollectionRole

router = APIRouter(prefix="/collections/{collection_id}/documents", tags=["documents"])


def _filters(collection_id: uuid.UUID, status: str | None, q: str):
    where = [Document.collection_id == collection_id]
    if status:
        where.append(Document.status == status)
    if q:
        pattern = f"%{q}%"
        where.append(or_(Document.title.ilike(pattern), Document.uri.ilike(pattern)))
    return where


@router.get("")
async def list_collection_documents(
    session: DbSession,
    collection: Collection = collection_access(CollectionRole.VIEWER),
    status: str | None = None,
    q: str = "",
    limit: int = 50,
    offset: int = 0,
) -> DocumentListOut:
    where = _filters(collection.id, status, q)
    total = (
        await session.execute(select(func.count()).select_from(Document).where(*where))
    ).scalar() or 0

    counts = (
        select(Chunk.document_id, func.count().label("n"))
        .where(Chunk.collection_id == collection.id)
        .group_by(Chunk.document_id)
        .subquery()
    )
    rows = await session.execute(
        select(Document, counts.c.n)
        .join(counts, counts.c.document_id == Document.id, isouter=True)
        .where(*where)
        .order_by(Document.updated_at.desc())
        .limit(min(limit, 200))
        .offset(max(offset, 0))
    )
    items = []
    for doc, chunk_count in rows:
        out = DocumentWithChunksOut.model_validate(doc)
        out.chunk_count = chunk_count or 0
        items.append(out)
    return DocumentListOut(items=items, total=total)


async def _get_document(
    session: DbSession, collection: Collection, document_id: uuid.UUID
) -> Document:
    document = await session.get(Document, document_id)
    if document is None or document.collection_id != collection.id:
        raise HTTPException(404, "Document not found")
    return document


@router.get("/{document_id}")
async def get_document(
    document_id: uuid.UUID,
    session: DbSession,
    collection: Collection = collection_access(CollectionRole.VIEWER),
) -> DocumentDetailOut:
    document = await _get_document(session, collection, document_id)
    out = DocumentDetailOut.model_validate(document)
    out.chunk_count = (
        await session.execute(
            select(func.count()).select_from(Chunk).where(Chunk.document_id == document.id)
        )
    ).scalar() or 0
    return out


@router.get("/{document_id}/chunks")
async def list_document_chunks(
    document_id: uuid.UUID,
    session: DbSession,
    collection: Collection = collection_access(CollectionRole.VIEWER),
    limit: int = 20,
    offset: int = 0,
) -> ChunkListOut:
    document = await _get_document(session, collection, document_id)
    total = (
        await session.execute(
            select(func.count()).select_from(Chunk).where(Chunk.document_id == document.id)
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
        .where(Chunk.document_id == document.id)
        .order_by(Chunk.ordinal)
        .limit(min(limit, 100))
        .offset(max(offset, 0))
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
