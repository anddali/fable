"""API key management (SSO sessions only — a key cannot mint keys).

Any signed-in user can create keys. Scopes are collection ids and must be
readable by the creator at creation time; at search time the key is limited to
the intersection of its scopes with what the owner can still read.
"""

from __future__ import annotations

import secrets
import uuid
from datetime import UTC, datetime, timedelta

from fastapi import APIRouter, HTTPException, status
from sqlalchemy import select
from sqlalchemy.orm import selectinload

from rag_api.auth import API_KEY_PREFIX, CurrentUser, hash_api_key
from rag_api.deps import DbSession
from rag_api.dto import ApiKeyCreate, ApiKeyCreated, ApiKeyOut
from rag_core.db.models import ApiKey, Collection
from rag_core.rbac import effective_collection_role

router = APIRouter(prefix="/api-keys", tags=["api-keys"])


async def _readable_collection_ids(session, principal) -> set[uuid.UUID]:
    result = await session.execute(select(Collection).options(selectinload(Collection.principals)))
    return {
        c.id
        for c in result.scalars()
        if principal.is_admin
        or effective_collection_role(principal, c.principals, public=c.visibility == "public")
    }


@router.post("", status_code=status.HTTP_201_CREATED)
async def create_api_key(
    body: ApiKeyCreate, session: DbSession, principal: CurrentUser
) -> ApiKeyCreated:
    readable = await _readable_collection_ids(session, principal)
    denied = [str(cid) for cid in body.collection_ids if cid not in readable]
    if denied:
        raise HTTPException(status.HTTP_404_NOT_FOUND, f"Unknown collection(s): {denied}")

    secret = API_KEY_PREFIX + secrets.token_hex(24)
    key = ApiKey(
        name=body.name,
        prefix=secret[:12],
        key_hash=hash_api_key(secret),
        owner_oid=principal.oid,
        owner_name=principal.name,
        scopes=sorted({str(cid) for cid in body.collection_ids}),
        expires_at=(
            datetime.now(UTC) + timedelta(days=body.expires_in_days)
            if body.expires_in_days
            else None
        ),
    )
    session.add(key)
    await session.commit()
    return ApiKeyCreated(**ApiKeyOut.model_validate(key).model_dump(), secret=secret)


@router.get("")
async def list_api_keys(
    session: DbSession, principal: CurrentUser, all: bool = False
) -> list[ApiKeyOut]:
    """Own keys; admins may pass ``all=true`` for every key on the platform."""
    query = select(ApiKey).order_by(ApiKey.created_at.desc())
    if not (all and principal.is_admin):
        query = query.where(ApiKey.owner_oid == principal.oid)
    result = await session.execute(query)
    return [ApiKeyOut.model_validate(k) for k in result.scalars()]


@router.delete("/{key_id}", status_code=status.HTTP_204_NO_CONTENT)
async def revoke_api_key(key_id: uuid.UUID, session: DbSession, principal: CurrentUser) -> None:
    key = await session.get(ApiKey, key_id)
    if key is None or (key.owner_oid != principal.oid and not principal.is_admin):
        raise HTTPException(status.HTTP_404_NOT_FOUND, "API key not found")
    key.revoked = True
    await session.commit()
