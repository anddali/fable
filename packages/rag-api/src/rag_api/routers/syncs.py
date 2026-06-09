from __future__ import annotations

import procrastinate
from fastapi import APIRouter, HTTPException, status
from sqlalchemy import select

from rag_api.deps import DbSession, collection_access
from rag_api.dto import SyncRunOut
from rag_core.db.models import Collection, SyncRun
from rag_core.queue import defer_sync
from rag_core.rbac import CollectionRole

router = APIRouter(prefix="/collections/{collection_id}/syncs", tags=["sync"])


@router.post("", status_code=status.HTTP_202_ACCEPTED)
async def trigger_sync(
    collection: Collection = collection_access(CollectionRole.EDITOR),
) -> dict:
    try:
        job_id = await defer_sync(str(collection.id), trigger="manual")
    except procrastinate.exceptions.AlreadyEnqueued as exc:
        raise HTTPException(
            status.HTTP_409_CONFLICT, "A sync for this collection is already queued"
        ) from exc
    return {"job_id": job_id, "status": "queued"}


@router.get("")
async def list_sync_runs(
    session: DbSession,
    collection: Collection = collection_access(CollectionRole.VIEWER),
    limit: int = 20,
) -> list[SyncRunOut]:
    result = await session.execute(
        select(SyncRun)
        .where(SyncRun.collection_id == collection.id)
        .order_by(SyncRun.started_at.desc())
        .limit(min(limit, 100))
    )
    return [SyncRunOut.model_validate(r) for r in result.scalars()]
