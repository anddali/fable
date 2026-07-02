"""Shared Procrastinate app (Postgres-backed job queue).

Defined in core so the API can *defer* jobs by task name without importing the
worker package; rag-worker imports this app and registers the actual task
implementations.

Task names (string contract between API and worker):
    rag_worker.tasks.sync_collection(collection_id, trigger)
    rag_worker.tasks.index_document(document_id)
"""

from __future__ import annotations

from functools import lru_cache

import procrastinate

from rag_core.config import get_settings

TASK_SYNC_COLLECTION = "rag_worker.tasks.sync_collection"
TASK_INDEX_DOCUMENT = "rag_worker.tasks.index_document"

QUEUE_SYNC = "sync"
QUEUE_INDEX = "index"
# Cron-beat dispatch (schedule_due_syncs) — kept off QUEUE_SYNC so a stuck sync
# job can never block the minute tick that schedules new ones.
QUEUE_BEAT = "beat"


@lru_cache
def get_app() -> procrastinate.App:
    settings = get_settings()
    return procrastinate.App(
        connector=procrastinate.PsycopgConnector(conninfo=settings.procrastinate_dsn),
        import_paths=["rag_worker.tasks"],
    )


async def defer_sync(collection_id: str, trigger: str = "manual") -> int:
    app = get_app()
    job = await app.configure_task(
        TASK_SYNC_COLLECTION,
        queue=QUEUE_SYNC,
        # One concurrent sync per collection; duplicates are rejected.
        queueing_lock=f"sync:{collection_id}",
    ).defer_async(collection_id=str(collection_id), trigger=trigger)
    return job


async def defer_index(document_id: str) -> int:
    app = get_app()
    job = await app.configure_task(
        TASK_INDEX_DOCUMENT,
        queue=QUEUE_INDEX,
        queueing_lock=f"index:{document_id}",
    ).defer_async(document_id=str(document_id))
    return job
