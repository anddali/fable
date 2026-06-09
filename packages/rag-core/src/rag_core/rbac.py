"""Authorization model.

Two layers:

1. **Global roles** (from Entra app roles, mapped via ``AuthSettings.role_map``):
   - ``admin``    full platform control
   - ``curator``  may create collections (becomes their owner)
   - ``reader``   may call the API at all; access is then per-collection

2. **Collection ACLs** (rows in ``collection_principals``), granting an Entra
   user or group object id a collection role:
   - ``owner``   manage ACLs + everything below
   - ``editor``  manage sources/uploads, trigger syncs + everything below
   - ``viewer``  search / read

Retrieval enforces the same model: a search only ever touches collections the
caller can view.
"""

from __future__ import annotations

from collections.abc import Iterable
from enum import StrEnum
from typing import Any

from rag_core.db.models import CollectionPrincipal
from rag_core.schemas import Principal


class GlobalRole(StrEnum):
    ADMIN = "admin"
    CURATOR = "curator"
    READER = "reader"


class CollectionRole(StrEnum):
    OWNER = "owner"
    EDITOR = "editor"
    VIEWER = "viewer"


_ROLE_ORDER = {
    CollectionRole.VIEWER: 1,
    CollectionRole.EDITOR: 2,
    CollectionRole.OWNER: 3,
}


class AuthorizationError(Exception):
    """Raised when a principal lacks the required permission (HTTP 403)."""


def map_global_roles(token_roles: Iterable[str], role_map: dict[str, str]) -> set[str]:
    """Map Entra app-role claim values to platform global roles."""
    return {role_map[r] for r in token_roles if r in role_map}


def effective_collection_role(
    principal: Principal, acl: Iterable[CollectionPrincipal | Any]
) -> CollectionRole | None:
    """Highest collection role granted to the principal directly or via groups."""
    if principal.is_admin:
        return CollectionRole.OWNER
    best: CollectionRole | None = None
    for entry in acl:
        matches = (entry.principal_type == "user" and entry.principal_id == principal.oid) or (
            entry.principal_type == "group" and entry.principal_id in principal.group_ids
        )
        if not matches:
            continue
        role = CollectionRole(entry.role)
        if best is None or _ROLE_ORDER[role] > _ROLE_ORDER[best]:
            best = role
    return best


def require_collection_role(
    principal: Principal,
    acl: Iterable[CollectionPrincipal | Any],
    required: CollectionRole,
) -> CollectionRole:
    actual = effective_collection_role(principal, acl)
    if actual is None or _ROLE_ORDER[actual] < _ROLE_ORDER[required]:
        raise AuthorizationError(f"requires collection role {required.value!r}")
    return actual


def can_create_collections(principal: Principal) -> bool:
    return bool({GlobalRole.ADMIN.value, GlobalRole.CURATOR.value} & principal.global_roles)
