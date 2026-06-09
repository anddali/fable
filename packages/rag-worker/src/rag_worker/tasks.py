"""Procrastinate task definitions.

Task names form the contract with ``rag_core.queue`` (used by the API to defer
jobs without importing this package) — keep them in sync.
"""

from __future__ import annotations

import uuid
from datetime import UTC, datetime

import structlog
from croniter import croniter
from sqlalchemy import select

from rag_core.blobstore import LocalBlobStore
from rag_core.config import get_settings
from rag_core.db.models import Collection
from rag_core.db.session import session_scope
from rag_core.pipeline.indexer import index_document as run_index
from rag_core.queue import QUEUE_INDEX, QUEUE_SYNC, defer_sync, get_app
from rag_worker.sync import run_collection_sync

log = structlog.get_logger(__name__)

app = get_app()


@app.task(
    name="rag_worker.tasks.sync_collection",
    queue=QUEUE_SYNC,
    retry=2,
    pass_context=True,
)
async def sync_collection(context, collection_id: str, trigger: str = "cron") -> None:
    log.info("task.sync_collection", collection_id=collection_id, trigger=trigger)
    await run_collection_sync(collection_id, trigger=trigger)


@app.task(
    name="rag_worker.tasks.index_document",
    queue=QUEUE_INDEX,
    retry=3,
)
async def index_document(document_id: str) -> None:
    settings = get_settings()
    blob_store = LocalBlobStore(settings.blob_dir)
    async with session_scope() as session:
        await run_index(session, settings, blob_store, uuid.UUID(document_id))


@app.periodic(cron="* * * * *")
@app.task(name="rag_worker.tasks.schedule_due_syncs", queueing_lock="beat:schedule_due_syncs")
async def schedule_due_syncs(timestamp: int) -> None:
    """Minute beat: enqueue syncs for collections whose cron schedule is due.

    ``next_sync_at`` is computed from the collection's cron expression and
    persisted, so each due tick fires exactly once even across worker restarts.
    """
    now = datetime.now(UTC)
    async with session_scope() as session:
        collections = (
            (await session.execute(select(Collection).where(Collection.sync_enabled.is_(True))))
            .scalars()
            .all()
        )
        for collection in collections:
            if not croniter.is_valid(collection.sync_cron):
                log.warning(
                    "beat.invalid_cron",
                    collection_id=str(collection.id),
                    cron=collection.sync_cron,
                )
                continue
            if collection.next_sync_at is None:
                # First time we see this collection/schedule: arm the timer.
                collection.next_sync_at = croniter(collection.sync_cron, now).get_next(datetime)
                continue
            if collection.next_sync_at <= now:
                collection.next_sync_at = croniter(collection.sync_cron, now).get_next(datetime)
                try:
                    await defer_sync(str(collection.id), trigger="cron")
                    log.info("beat.sync_enqueued", collection_id=str(collection.id))
                except Exception:  # AlreadyEnqueued: previous sync still queued
                    log.info("beat.sync_already_queued", collection_id=str(collection.id))
