from __future__ import annotations

import time
import uuid

from fastapi import APIRouter, HTTPException, status
from sqlalchemy import select
from sqlalchemy.orm import selectinload

from rag_api.auth import CurrentPrincipal
from rag_api.deps import DbSession, SettingsDep
from rag_api.dto import SearchRequest, SearchResponse
from rag_core.db.models import Collection, QueryLog
from rag_core.rbac import effective_collection_role
from rag_core.retrieval.service import search as run_search

router = APIRouter(tags=["search"])


@router.post("/search")
async def search(
    body: SearchRequest,
    session: DbSession,
    settings: SettingsDep,
    principal: CurrentPrincipal,
) -> SearchResponse:
    # Resolve the set of collections the caller may read; the retrieval query
    # is hard-filtered to exactly this set. API keys are further restricted to
    # the intersection with their scopes (so a key never outlives its owner's
    # direct access, and never exceeds its declared scope).
    result = await session.execute(select(Collection).options(selectinload(Collection.principals)))
    readable = {
        c.id: c
        for c in result.scalars()
        if principal.is_admin
        or effective_collection_role(principal, c.principals, public=c.visibility == "public")
    }
    if principal.key_scopes is not None:
        readable = {cid: c for cid, c in readable.items() if str(cid) in principal.key_scopes}

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
