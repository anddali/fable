from __future__ import annotations

from collections.abc import AsyncIterator
from typing import ClassVar

from sqlalchemy import select

from rag_core.db.models import UploadItem
from rag_core.interfaces import ConnectorPlugin, SyncContext
from rag_core.schemas import RawDocument


class UploadConnector(ConnectorPlugin):
    """Surfaces manually uploaded files (staged via the API) to the sync engine.

    The upload endpoint writes the file to the blob store and records an
    UploadItem row; this connector simply enumerates non-deleted items, so
    deletions in the API propagate to the index on the next sync.
    """

    name: ClassVar[str] = "upload"

    async def iter_documents(self, ctx: SyncContext) -> AsyncIterator[RawDocument]:
        result = await ctx.session.execute(
            select(UploadItem).where(
                UploadItem.source_id == ctx.source_id,
                UploadItem.deleted.is_(False),
            )
        )
        for item in result.scalars():
            yield RawDocument(
                external_id=str(item.id),
                title=item.filename,
                uri=f"upload://{item.id}/{item.filename}",
                mime_type=item.mime_type,
                content=ctx.blob_store.get(item.blob_key),
                content_hash=item.content_hash,
                source_updated_at=item.updated_at,
                metadata={"filename": item.filename, "uploaded_by": item.uploaded_by},
            )
