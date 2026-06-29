from __future__ import annotations

import hashlib
import uuid
from pathlib import PurePosixPath

from fastapi import APIRouter, HTTPException, UploadFile, status
from sqlalchemy import select

from rag_api.auth import CurrentUser
from rag_api.deps import BlobStoreDep, DbSession, SettingsDep, collection_access
from rag_api.dto import UploadItemOut
from rag_core.db.models import Collection, Source, UploadItem
from rag_core.rbac import CollectionRole

router = APIRouter(
    prefix="/collections/{collection_id}/sources/{source_id}/files", tags=["uploads"]
)

# MVP allow-list; extend alongside new parser plugins.
ALLOWED_MIME = {
    ".pdf": "application/pdf",
    ".txt": "text/plain",
    ".md": "text/markdown",
    ".markdown": "text/markdown",
    ".csv": "text/csv",
    ".xlsx": "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ".docx": "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ".pptx": "application/vnd.openxmlformats-officedocument.presentationml.presentation",
}


async def _get_upload_source(session, collection_id: uuid.UUID, source_id: uuid.UUID) -> Source:
    source = await session.get(Source, source_id)
    if source is None or source.collection_id != collection_id:
        raise HTTPException(status.HTTP_404_NOT_FOUND, "Source not found")
    if source.connector != "upload":
        raise HTTPException(
            status.HTTP_422_UNPROCESSABLE_ENTITY,
            f"Source uses connector {source.connector!r}; files can only be added to 'upload' sources",
        )
    return source


@router.post("", status_code=status.HTTP_201_CREATED)
async def upload_file(
    source_id: uuid.UUID,
    file: UploadFile,
    session: DbSession,
    settings: SettingsDep,
    blob_store: BlobStoreDep,
    principal: CurrentUser,
    collection: Collection = collection_access(CollectionRole.EDITOR),
) -> UploadItemOut:
    source = await _get_upload_source(session, collection.id, source_id)

    filename = PurePosixPath(file.filename or "upload").name
    suffix = PurePosixPath(filename).suffix.lower()
    if suffix not in ALLOWED_MIME:
        raise HTTPException(
            status.HTTP_415_UNSUPPORTED_MEDIA_TYPE,
            f"Unsupported file type {suffix!r}; allowed: {sorted(ALLOWED_MIME)}",
        )

    data = await file.read()
    if len(data) > settings.max_upload_bytes:
        raise HTTPException(
            status.HTTP_413_REQUEST_ENTITY_TOO_LARGE,
            f"File exceeds {settings.max_upload_bytes} bytes",
        )

    content_hash = hashlib.sha256(data).hexdigest()
    item_id = uuid.uuid4()
    blob_key = f"uploads/{source.id}/{item_id}/{content_hash}"
    blob_store.put(blob_key, data)

    item = UploadItem(
        id=item_id,
        source_id=source.id,
        filename=filename,
        mime_type=ALLOWED_MIME[suffix],
        size_bytes=len(data),
        blob_key=blob_key,
        content_hash=content_hash,
        uploaded_by=principal.oid,
    )
    session.add(item)
    await session.commit()
    return UploadItemOut.model_validate(item)


@router.get("")
async def list_files(
    source_id: uuid.UUID,
    session: DbSession,
    collection: Collection = collection_access(CollectionRole.VIEWER),
) -> list[UploadItemOut]:
    await _get_upload_source(session, collection.id, source_id)
    result = await session.execute(
        select(UploadItem)
        .where(UploadItem.source_id == source_id, UploadItem.deleted.is_(False))
        .order_by(UploadItem.created_at.desc())
    )
    return [UploadItemOut.model_validate(i) for i in result.scalars()]


@router.delete("/{item_id}", status_code=status.HTTP_204_NO_CONTENT)
async def delete_file(
    source_id: uuid.UUID,
    item_id: uuid.UUID,
    session: DbSession,
    blob_store: BlobStoreDep,
    collection: Collection = collection_access(CollectionRole.EDITOR),
) -> None:
    await _get_upload_source(session, collection.id, source_id)
    item = await session.get(UploadItem, item_id)
    if item is None or item.source_id != source_id or item.deleted:
        raise HTTPException(status.HTTP_404_NOT_FOUND, "File not found")
    # Soft delete: the next sync tombstones the document and removes chunks.
    item.deleted = True
    await session.commit()
    # The connector skips deleted items, so it never reads this staging blob
    # again — drop it now so it doesn't orphan (the document's canonical copy
    # is removed separately when sync tombstones the document).
    blob_store.delete(item.blob_key)
