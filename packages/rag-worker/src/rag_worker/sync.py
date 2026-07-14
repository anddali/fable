"""Collection sync engine.

For each enabled source of a collection:
  1. instantiate its connector plugin and iterate RawDocuments,
  2. delta-detect against stored content hashes (unchanged -> skip),
  3. persist payloads to the blob store and upsert document rows,
  4. enqueue indexing jobs for new/changed documents,
  5. tombstone documents that disappeared from the source (full-enumeration
     connectors only) and drop their chunks.

Each source is processed in its own transaction; one failing source never
poisons the others or the overall run.
"""

from __future__ import annotations

import uuid
from datetime import UTC, datetime
from typing import Any

import structlog
from sqlalchemy import select
from sqlalchemy.ext.asyncio import AsyncSession

from rag_core import plugins
from rag_core.blobstore import BlobStore, LocalBlobStore, document_blob_key
from rag_core.config import get_settings
from rag_core.db.models import Collection, Document, Source, SyncRun
from rag_core.db.session import session_scope
from rag_core.interfaces import ConnectorPlugin, SyncContext
from rag_core.pipeline.indexer import remove_document_chunks
from rag_core.queue import defer_index

log = structlog.get_logger(__name__)


async def run_collection_sync(collection_id: str, trigger: str = "cron") -> dict[str, Any]:
    settings = get_settings()
    blob_store = LocalBlobStore(settings.blob_dir)
    cid = uuid.UUID(collection_id)

    async with session_scope() as session:
        collection = await session.get(Collection, cid)
        if collection is None:
            log.warning("sync.collection_missing", collection_id=collection_id)
            return {}
        run = SyncRun(collection_id=cid, trigger=trigger, started_at=datetime.now(UTC))
        session.add(run)
        await session.flush()
        run_id = run.id
        source_ids = [
            s.id
            for s in (
                await session.execute(
                    select(Source).where(Source.collection_id == cid, Source.enabled.is_(True))
                )
            ).scalars()
        ]

    stats: dict[str, Any] = {}
    failed = False
    for source_id in source_ids:
        try:
            async with session_scope() as session:
                source = await session.get(Source, source_id)
                counts, to_index = await _sync_source(session, blob_store, source)
                stats[source.name] = counts
        except Exception as exc:
            failed = True
            log.exception("sync.source_failed", source_id=str(source_id))
            stats[str(source_id)] = {"error": str(exc)[:2000]}
            continue
        # Enqueue only after the session_scope above has committed: the queue
        # runs on its own connection, so a job deferred mid-transaction can be
        # picked up before the document rows it needs are visible.
        for document_id in to_index:
            try:
                await defer_index(str(document_id))
            except Exception:  # AlreadyEnqueued -> an index job is already pending
                log.info("sync.index_already_queued", document_id=str(document_id))

    async with session_scope() as session:
        run = await session.get(SyncRun, run_id)
        run.status = "failed" if failed else "succeeded"
        run.finished_at = datetime.now(UTC)
        run.stats = stats
        if failed:
            run.error = "one or more sources failed; see stats"
    log.info("sync.done", collection_id=collection_id, failed=failed, stats=stats)
    return stats


async def _sync_source(
    session: AsyncSession, blob_store: BlobStore, source: Source
) -> tuple[dict[str, Any], list[uuid.UUID]]:
    """Sync one source; returns (counts, document ids to index).

    Deliberately does NOT enqueue index jobs — the caller defers them after
    this session's transaction commits, so index workers never race ahead of
    the document rows.
    """
    connector: ConnectorPlugin = plugins.create("connector", source.connector, source.config)
    ctx = SyncContext(
        settings=get_settings(),
        session=session,
        blob_store=blob_store,
        source_id=source.id,
        collection_id=source.collection_id,
        state=dict(source.state or {}),
    )

    existing = {
        doc.external_id: doc
        for doc in (
            await session.execute(select(Document).where(Document.source_id == source.id))
        ).scalars()
    }
    seen: set[str] = set()
    counts = {"seen": 0, "added": 0, "updated": 0, "unchanged": 0, "deleted": 0}
    to_index: list[uuid.UUID] = []

    async for raw in connector.iter_documents(ctx):
        raw.ensure_hash()
        counts["seen"] += 1
        seen.add(raw.external_id)
        doc = existing.get(raw.external_id)

        if doc is not None and doc.content_hash == raw.content_hash and doc.status == "indexed":
            counts["unchanged"] += 1
            continue

        if doc is None:
            doc = Document(
                source_id=source.id,
                collection_id=source.collection_id,
                external_id=raw.external_id,
            )
            session.add(doc)
            await session.flush()
            counts["added"] += 1
        else:
            counts["updated"] += 1

        payload = raw.payload()
        old_blob_key = doc.blob_key
        blob_key = document_blob_key(source.collection_id, doc.id, raw.content_hash)
        blob_store.put(blob_key, payload)

        doc.title = raw.title or doc.title
        doc.uri = raw.uri
        doc.mime_type = raw.mime_type
        doc.content_hash = raw.content_hash
        doc.size_bytes = len(payload)
        doc.blob_key = blob_key
        doc.doc_metadata = raw.metadata
        doc.status = "pending"
        doc.error = ""
        doc.source_updated_at = raw.source_updated_at
        to_index.append(doc.id)

        # Content changed: drop the superseded payload so blobs don't orphan.
        if old_blob_key and old_blob_key != blob_key:
            blob_store.delete(old_blob_key)

    # Tombstone vanished documents (only safe with full enumeration).
    if not type(connector).incremental:
        for external_id, doc in existing.items():
            if external_id not in seen and doc.status != "deleted":
                doc.status = "deleted"
                await remove_document_chunks(session, doc.id, blob_store)
                blob_store.delete(doc.blob_key)
                counts["deleted"] += 1

    source.state = ctx.state
    source.last_synced_at = datetime.now(UTC)
    await session.flush()

    return counts, to_index
