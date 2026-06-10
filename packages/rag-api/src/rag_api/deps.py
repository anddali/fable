"""Shared FastAPI dependencies: DB session, blob store, collection access checks."""

from __future__ import annotations

import uuid
from collections.abc import AsyncIterator
from functools import lru_cache
from typing import Annotated

from fastapi import Depends, HTTPException, status
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy.orm import selectinload

from rag_api.auth import CurrentUser
from rag_core.blobstore import BlobStore, LocalBlobStore
from rag_core.config import Settings, get_settings
from rag_core.db.models import Collection
from rag_core.db.session import get_session_factory
from rag_core.rbac import AuthorizationError, CollectionRole, require_collection_role
from rag_core.schemas import Principal

SettingsDep = Annotated[Settings, Depends(get_settings)]


async def get_db() -> AsyncIterator[AsyncSession]:
    async with get_session_factory()() as session:
        yield session


DbSession = Annotated[AsyncSession, Depends(get_db)]


@lru_cache
def _blob_store(blob_dir: str) -> BlobStore:
    return LocalBlobStore(blob_dir)


def get_blob_store(settings: SettingsDep) -> BlobStore:
    return _blob_store(settings.blob_dir)


BlobStoreDep = Annotated[BlobStore, Depends(get_blob_store)]


async def load_collection(session: AsyncSession, collection_id: uuid.UUID) -> Collection:
    result = await session.get(
        Collection, collection_id, options=[selectinload(Collection.principals)]
    )
    if result is None:
        raise HTTPException(status.HTTP_404_NOT_FOUND, "Collection not found")
    return result


def check_collection_access(
    principal: Principal, collection: Collection, required: CollectionRole
) -> None:
    public = collection.visibility == "public"
    try:
        require_collection_role(principal, collection.principals, required, public=public)
    except AuthorizationError as exc:
        # 404 for viewers without any access (don't leak existence), 403 when
        # they can see it but lack the higher role.
        from rag_core.rbac import effective_collection_role

        if effective_collection_role(principal, collection.principals, public=public) is None:
            raise HTTPException(status.HTTP_404_NOT_FOUND, "Collection not found") from exc
        raise HTTPException(status.HTTP_403_FORBIDDEN, str(exc)) from exc


def collection_access(required: CollectionRole):
    """Path-param dependency: loads the collection and enforces the role.

    SSO-only by design: API keys are confined to /search.
    """

    async def _dep(
        collection_id: uuid.UUID, session: DbSession, principal: CurrentUser
    ) -> Collection:
        collection = await load_collection(session, collection_id)
        check_collection_access(principal, collection, required)
        return collection

    return Depends(_dep)
