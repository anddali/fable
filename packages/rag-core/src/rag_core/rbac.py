"""Authorization model.

Two layers:

1. **Global roles** (from Entra app roles, mapped via ``AuthSettings.role_map``):
   - ``admin``  full platform control, implicit owner of every collection
   - ``user``   regular user: may create collections (becoming their owner),
                access their own private collections and any public collection

2. **Collection ACLs** (rows in ``collection_principals``), granting an Entra
   user or group object id a collection role:
   - ``owner``   manage ACLs + everything below
   - ``editor``  manage sources/uploads, trigger syncs + everything below
   - ``viewer``  search / read

Collections also carry a ``visibility``: ``private`` (ACL only) or ``public``
(every authenticated principal gets implicit ``viewer``).

API-key principals are derived from their owning user (oid only, no group
claims) and additionally restricted to the key's collection scopes at search
time; they can never reach management endpoints.
"""

from __future__ import annotations

from collections.abc import Iterable
from enum import StrEnum
from typing import Any

from rag_core.db.models import CollectionPrincipal
from rag_core.schemas import Principal


class GlobalRole(StrEnum):
    ADMIN = "admin"
    USER = "user"


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
    principal: Principal,
    acl: Iterable[CollectionPrincipal | Any],
    *,
    public: bool = False,
) -> CollectionRole | None:
    """Highest collection role granted directly, via groups, or via visibility."""
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
    if best is None and public:
        return CollectionRole.VIEWER
    return best


def require_collection_role(
    principal: Principal,
    acl: Iterable[CollectionPrincipal | Any],
    required: CollectionRole,
    *,
    public: bool = False,
) -> CollectionRole:
    actual = effective_collection_role(principal, acl, public=public)
    if actual is None or _ROLE_ORDER[actual] < _ROLE_ORDER[required]:
        raise AuthorizationError(f"requires collection role {required.value!r}")
    return actual


def can_create_collections(principal: Principal) -> bool:
    # Any interactively signed-in user may create collections; API keys can't.
    return principal.auth_method == "sso" and bool(
        {GlobalRole.ADMIN.value, GlobalRole.USER.value} & principal.global_roles
    )
