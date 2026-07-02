from __future__ import annotations

import uuid

import httpx
from fastapi import APIRouter, HTTPException, status
from pydantic import ValidationError
from sqlalchemy import select

from rag_api.deps import BlobStoreDep, DbSession, SettingsDep, collection_access
from rag_api.dto import (
    DocumentOut,
    SourceCreate,
    SourceOut,
    SourceTestRequest,
    SourceTestResult,
    SourceUpdate,
)
from rag_core import plugins
from rag_core.config import Settings
from rag_core.db.models import Collection, Document, Source
from rag_core.interfaces import SyncContext
from rag_core.rbac import CollectionRole

router = APIRouter(prefix="/collections/{collection_id}/sources", tags=["sources"])


def _connector_class(connector: str):
    try:
        return plugins.get_class("connector", connector)
    except plugins.PluginNotFoundError as exc:
        raise HTTPException(status.HTTP_422_UNPROCESSABLE_ENTITY, str(exc)) from exc


def _validate_connector(connector: str, config: dict) -> dict:
    cls = _connector_class(connector)
    try:
        return cls.config_model.model_validate(config).model_dump()
    except ValidationError as exc:
        # Field-path + message, not a raw pydantic dump.
        errors = [
            {"field": ".".join(str(p) for p in e["loc"]) or "(root)", "message": e["msg"]}
            for e in exc.errors()
        ]
        raise HTTPException(
            status.HTTP_422_UNPROCESSABLE_ENTITY,
            {"message": f"Invalid config for connector {connector!r}", "errors": errors},
        ) from exc


async def _resolve_link(connector: str, link: str, include_children: bool, settings: Settings) -> dict:
    """Turn a pasted URL into a config fragment via the connector's URL parser.

    Follows redirects first (Confluence short ``/x/`` links) and, for connectors
    with a configured connection host, rejects links pointing elsewhere.
    """
    cls = _connector_class(connector)
    if not cls.supports_url_targets:  # type: ignore[attr-defined]
        raise HTTPException(
            status.HTTP_422_UNPROCESSABLE_ENTITY,
            f"Connector {connector!r} does not accept a link",
        )
    resolved = link
    try:
        async with httpx.AsyncClient(follow_redirects=True, timeout=15) as client:
            resp = await client.get(link)
            resolved = str(resp.url)
    except httpx.HTTPError:
        pass  # unreachable/short-link resolution failed; parse the raw link
    fragment = cls.parse_target_url(resolved, settings)  # type: ignore[attr-defined]
    if fragment is None:
        raise HTTPException(
            status.HTTP_422_UNPROCESSABLE_ENTITY,
            f"Could not recognize a {connector} target in {link!r}",
        )
    if include_children:
        for target in fragment.get("targets", []):
            if target.get("kind") == "page":
                target["kind"] = "tree"
    return fragment


async def _get_source(session, collection_id: uuid.UUID, source_id: uuid.UUID) -> Source:
    source = await session.get(Source, source_id)
    if source is None or source.collection_id != collection_id:
        raise HTTPException(status.HTTP_404_NOT_FOUND, "Source not found")
    return source


@router.post("/test")
async def test_source(
    body: SourceTestRequest,
    session: DbSession,
    settings: SettingsDep,
    blob_store: BlobStoreDep,
    collection: Collection = collection_access(CollectionRole.EDITOR),
) -> SourceTestResult:
    """Dry-run a connector config before creating a source.

    Resolves a pasted ``link`` into config, validates shape, checks the connector
    is configured, and pulls one document. Returns the resolved config plus an
    ok/error detail — so failures surface here, not at the next sync.
    """
    config = dict(body.config)
    if body.link:
        config = {**config, **await _resolve_link(body.connector, body.link, body.include_children, settings)}
    try:
        config = _validate_connector(body.connector, config)
    except HTTPException as exc:
        return SourceTestResult(ok=False, config=config, detail=str(exc.detail))
    connector = plugins.create("connector", body.connector, config)
    if not connector.ready(settings):
        return SourceTestResult(
            ok=False, config=config, detail=f"Connector {body.connector!r} is not configured"
        )
    ctx = SyncContext(
        settings=settings,
        session=session,
        blob_store=blob_store,
        source_id=uuid.uuid4(),
        collection_id=collection.id,
    )
    try:
        first = await anext(aiter(connector.iter_documents(ctx)), None)
    except Exception as exc:  # noqa: BLE001 — report any connector error to the caller
        return SourceTestResult(ok=False, config=config, detail=str(exc))
    if first is None:
        return SourceTestResult(ok=True, config=config, detail="Connected — no documents matched")
    return SourceTestResult(ok=True, config=config, detail=f"OK — sample page: {first.title!r}")


@router.post("", status_code=status.HTTP_201_CREATED)
async def create_source(
    body: SourceCreate,
    session: DbSession,
    settings: SettingsDep,
    collection: Collection = collection_access(CollectionRole.EDITOR),
) -> SourceOut:
    config = dict(body.config)
    if body.link:
        config = {**config, **await _resolve_link(body.connector, body.link, body.include_children, settings)}
    config = _validate_connector(body.connector, config)
    if not _connector_class(body.connector).ready(settings):  # type: ignore[attr-defined]
        raise HTTPException(
            status.HTTP_422_UNPROCESSABLE_ENTITY,
            f"Connector {body.connector!r} is not configured on this server",
        )
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
