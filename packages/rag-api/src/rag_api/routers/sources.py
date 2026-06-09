from __future__ import annotations

import uuid

from fastapi import APIRouter, HTTPException, status
from pydantic import ValidationError
from sqlalchemy import select

from rag_api.deps import DbSession, collection_access
from rag_api.dto import DocumentOut, SourceCreate, SourceOut, SourceUpdate
from rag_core import plugins
from rag_core.db.models import Collection, Document, Source
from rag_core.rbac import CollectionRole

router = APIRouter(prefix="/collections/{collection_id}/sources", tags=["sources"])


def _validate_connector(connector: str, config: dict) -> dict:
    try:
        cls = plugins.get_class("connector", connector)
    except plugins.PluginNotFoundError as exc:
        raise HTTPException(status.HTTP_422_UNPROCESSABLE_ENTITY, str(exc)) from exc
    try:
        return cls.config_model.model_validate(config).model_dump()
    except ValidationError as exc:
        raise HTTPException(
            status.HTTP_422_UNPROCESSABLE_ENTITY,
            f"Invalid config for connector {connector!r}: {exc.errors()}",
        ) from exc


async def _get_source(session, collection_id: uuid.UUID, source_id: uuid.UUID) -> Source:
    source = await session.get(Source, source_id)
    if source is None or source.collection_id != collection_id:
        raise HTTPException(status.HTTP_404_NOT_FOUND, "Source not found")
    return source


@router.post("", status_code=status.HTTP_201_CREATED)
async def create_source(
    body: SourceCreate,
    session: DbSession,
    collection: Collection = collection_access(CollectionRole.EDITOR),
) -> SourceOut:
    config = _validate_connector(body.connector, body.config)
    existing = await session.execute(
        select(Source.id).where(Source.collection_id == collection.id, Source.name == body.name)
    )
    if existing.scalar() is not None:
        raise HTTPException(status.HTTP_409_CONFLICT, f"Source {body.name!r} already exists")
    source = Source(
        collection_id=collection.id,
        name=body.name,
        connector=body.connector,
        config=config,
        enabled=body.enabled,
    )
    session.add(source)
    await session.commit()
    return SourceOut.model_validate(source)


@router.get("")
async def list_sources(
    session: DbSession,
    collection: Collection = collection_access(CollectionRole.VIEWER),
) -> list[SourceOut]:
    result = await session.execute(
        select(Source).where(Source.collection_id == collection.id).order_by(Source.name)
    )
    return [SourceOut.model_validate(s) for s in result.scalars()]


@router.patch("/{source_id}")
async def update_source(
    source_id: uuid.UUID,
    body: SourceUpdate,
    session: DbSession,
    collection: Collection = collection_access(CollectionRole.EDITOR),
) -> SourceOut:
    source = await _get_source(session, collection.id, source_id)
    if body.name is not None:
        source.name = body.name
    if body.config is not None:
        source.config = _validate_connector(source.connector, body.config)
    if body.enabled is not None:
        source.enabled = body.enabled
    await session.commit()
    return SourceOut.model_validate(source)


@router.delete("/{source_id}", status_code=status.HTTP_204_NO_CONTENT)
async def delete_source(
    source_id: uuid.UUID,
    session: DbSession,
    collection: Collection = collection_access(CollectionRole.EDITOR),
) -> None:
    source = await _get_source(session, collection.id, source_id)
    await session.delete(source)
    await session.commit()


@router.get("/{source_id}/documents")
async def list_documents(
    source_id: uuid.UUID,
    session: DbSession,
    collection: Collection = collection_access(CollectionRole.VIEWER),
    limit: int = 100,
    offset: int = 0,
) -> list[DocumentOut]:
    await _get_source(session, collection.id, source_id)
    result = await session.execute(
        select(Document)
        .where(Document.source_id == source_id)
        .order_by(Document.updated_at.desc())
        .limit(min(limit, 500))
        .offset(offset)
    )
    return [DocumentOut.model_validate(d) for d in result.scalars()]
