from __future__ import annotations

from fastapi import APIRouter, HTTPException, status
from sqlalchemy import select
from sqlalchemy.orm import selectinload

from rag_api.auth import CurrentUser
from rag_api.deps import BlobStoreDep, DbSession, collection_access, load_collection
from rag_api.dto import AclEntryIn, AclEntryOut, CollectionCreate, CollectionOut, CollectionUpdate
from rag_core.db.models import Collection, CollectionPrincipal
from rag_core.pipeline.indexer import collection_blob_keys
from rag_core.rbac import CollectionRole, can_create_collections, effective_collection_role

router = APIRouter(prefix="/collections", tags=["collections"])


def _out(collection: Collection, principal) -> CollectionOut:
    out = CollectionOut.model_validate(collection)
    role = effective_collection_role(
        principal, collection.principals, public=collection.visibility == "public"
    )
    out.my_role = role.value if role else None
    return out


@router.post("", status_code=status.HTTP_201_CREATED)
async def create_collection(
    body: CollectionCreate, session: DbSession, principal: CurrentUser
) -> CollectionOut:
    if not can_create_collections(principal):
        raise HTTPException(status.HTTP_403_FORBIDDEN, "Requires an interactive user session")
    existing = await session.execute(select(Collection.id).where(Collection.name == body.name))
    if existing.scalar() is not None:
        raise HTTPException(status.HTTP_409_CONFLICT, f"Collection {body.name!r} already exists")

    collection = Collection(
        name=body.name,
        description=body.description,
        visibility=body.visibility,
        pipeline=body.pipeline.model_dump(),
        sync_cron=body.sync_cron,
        sync_enabled=body.sync_enabled,
        created_by=principal.oid,
    )
    # Creator becomes owner.
    collection.principals.append(
        CollectionPrincipal(principal_type="user", principal_id=principal.oid, role="owner")
    )
    session.add(collection)
    await session.commit()
    await session.refresh(collection, attribute_names=["principals"])
    return _out(collection, principal)


@router.get("")
async def list_collections(session: DbSession, principal: CurrentUser) -> list[CollectionOut]:
    result = await session.execute(
        select(Collection).options(selectinload(Collection.principals)).order_by(Collection.name)
    )
    collections = result.scalars().all()
    visible = [
        c
        for c in collections
        if principal.is_admin
        or effective_collection_role(principal, c.principals, public=c.visibility == "public")
    ]
    return [_out(c, principal) for c in visible]


@router.get("/{collection_id}")
async def get_collection(
    session: DbSession,
    principal: CurrentUser,
    collection: Collection = collection_access(CollectionRole.VIEWER),
) -> CollectionOut:
    return _out(collection, principal)


@router.patch("/{collection_id}")
async def update_collection(
    body: CollectionUpdate,
    session: DbSession,
    principal: CurrentUser,
    collection: Collection = collection_access(CollectionRole.OWNER),
) -> CollectionOut:
    if body.description is not None:
        collection.description = body.description
    if body.visibility is not None:
        collection.visibility = body.visibility
    if body.pipeline is not None:
        collection.pipeline = body.pipeline.model_dump()
    if body.sync_cron is not None:
        collection.sync_cron = body.sync_cron
        collection.next_sync_at = None  # recomputed by the worker beat
    if body.sync_enabled is not None:
        collection.sync_enabled = body.sync_enabled
    await session.commit()
    return _out(collection, principal)


@router.delete("/{collection_id}", status_code=status.HTTP_204_NO_CONTENT)
async def delete_collection(
    session: DbSession,
    blob_store: BlobStoreDep,
    collection: Collection = collection_access(CollectionRole.OWNER),
) -> None:
    # Collect blob keys before the rows cascade away; the DB ON DELETE CASCADE
    # only removes rows, so payloads and figures would otherwise orphan on disk.
    blob_keys = await collection_blob_keys(session, collection.id)
    await session.delete(collection)
    await session.commit()
    # Only after the row delete commits do we touch the store, so a failed
    # delete never strands the DB pointing at missing blobs.
    for key in blob_keys:
        blob_store.delete(key)


# --- ACL management ---------------------------------------------------------


@router.get("/{collection_id}/permissions")
async def list_permissions(
    collection: Collection = collection_access(CollectionRole.OWNER),
) -> list[AclEntryOut]:
    return [AclEntryOut.model_validate(p) for p in collection.principals]


@router.put("/{collection_id}/permissions")
async def replace_permissions(
    entries: list[AclEntryIn],
    session: DbSession,
    principal: CurrentUser,
    collection: Collection = collection_access(CollectionRole.OWNER),
) -> list[AclEntryOut]:
    if not any(e.role == "owner" for e in entries):
        raise HTTPException(status.HTTP_422_UNPROCESSABLE_ENTITY, "At least one owner required")
    collection.principals.clear()
    for entry in entries:
        collection.principals.append(
            CollectionPrincipal(
                principal_type=entry.principal_type,
                principal_id=entry.principal_id,
                role=entry.role,
            )
        )
    await session.commit()
    refreshed = await load_collection(session, collection.id)
    return [AclEntryOut.model_validate(p) for p in refreshed.principals]
