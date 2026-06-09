"""Retrieval: vector (pgvector cosine) and hybrid (vector + Postgres FTS, RRF-fused).

ACL enforcement happens in the API layer, which passes only collection ids the
caller may view; every query here is hard-filtered to those ids.
"""

from __future__ import annotations

import uuid
from typing import Literal

from sqlalchemy import bindparam, text
from sqlalchemy.ext.asyncio import AsyncSession

from rag_core.config import Settings
from rag_core.pipeline.indexer import get_embedder
from rag_core.schemas import SearchHit

SearchMode = Literal["vector", "hybrid"]

_VECTOR_SQL = text(
    """
    SELECT c.id AS chunk_id, c.document_id, c.collection_id, c.ordinal, c.text,
           c.chunk_metadata, d.title, d.uri,
           1 - (c.embedding <=> :query_vec) AS score
    FROM chunks c
    JOIN documents d ON d.id = c.document_id AND d.status = 'indexed'
    WHERE c.collection_id IN :collection_ids
    ORDER BY c.embedding <=> :query_vec
    LIMIT :limit
    """
).bindparams(bindparam("collection_ids", expanding=True))

_LEXICAL_SQL = text(
    """
    SELECT c.id AS chunk_id, c.document_id, c.collection_id, c.ordinal, c.text,
           c.chunk_metadata, d.title, d.uri,
           ts_rank(c.tsv, websearch_to_tsquery('english', :query)) AS score
    FROM chunks c
    JOIN documents d ON d.id = c.document_id AND d.status = 'indexed'
    WHERE c.collection_id IN :collection_ids
      AND c.tsv @@ websearch_to_tsquery('english', :query)
    ORDER BY score DESC
    LIMIT :limit
    """
).bindparams(bindparam("collection_ids", expanding=True))


def _to_hit(row, score: float) -> SearchHit:
    return SearchHit(
        chunk_id=row.chunk_id,
        document_id=row.document_id,
        collection_id=row.collection_id,
        score=score,
        text=row.text,
        title=row.title,
        uri=row.uri,
        ordinal=row.ordinal,
        metadata=row.chunk_metadata or {},
    )


async def search(
    session: AsyncSession,
    settings: Settings,
    *,
    query: str,
    collection_ids: list[uuid.UUID],
    top_k: int = 8,
    mode: SearchMode = "hybrid",
) -> list[SearchHit]:
    if not collection_ids:
        return []
    top_k = min(top_k, settings.search_max_top_k)

    embedder = get_embedder(settings)
    query_vec = (await embedder.embed([query]))[0]
    vec_literal = "[" + ",".join(f"{v:.7f}" for v in query_vec) + "]"

    fetch = top_k * 4 if mode == "hybrid" else top_k
    vec_rows = (
        await session.execute(
            _VECTOR_SQL,
            {"query_vec": vec_literal, "collection_ids": collection_ids, "limit": fetch},
        )
    ).all()

    if mode == "vector":
        return [_to_hit(r, float(r.score)) for r in vec_rows[:top_k]]

    lex_rows = (
        await session.execute(
            _LEXICAL_SQL,
            {"query": query, "collection_ids": collection_ids, "limit": fetch},
        )
    ).all()

    # Reciprocal Rank Fusion (k=60).
    k = 60
    fused: dict[uuid.UUID, tuple[float, object]] = {}
    for rows in (vec_rows, lex_rows):
        for rank, row in enumerate(rows):
            score = 1.0 / (k + rank + 1)
            prev = fused.get(row.chunk_id)
            fused[row.chunk_id] = (score + (prev[0] if prev else 0.0), row)

    ranked = sorted(fused.values(), key=lambda item: item[0], reverse=True)[:top_k]
    return [_to_hit(row, score) for score, row in ranked]
