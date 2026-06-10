from dataclasses import dataclass

import pytest

from rag_core.rbac import (
    AuthorizationError,
    CollectionRole,
    can_create_collections,
    effective_collection_role,
    map_global_roles,
    require_collection_role,
)
from rag_core.schemas import Principal


@dataclass
class AclRow:
    principal_type: str
    principal_id: str
    role: str


def user(oid="u1", roles=("user",), groups=(), **kw) -> Principal:
    return Principal(oid=oid, global_roles=set(roles), group_ids=set(groups), **kw)


def test_map_global_roles():
    role_map = {"Rag.Admin": "admin", "Rag.User": "user"}
    assert map_global_roles(["Rag.Admin", "Unknown.Role"], role_map) == {"admin"}
    assert map_global_roles([], role_map) == set()


def test_direct_user_grant():
    acl = [AclRow("user", "u1", "editor")]
    assert effective_collection_role(user(), acl) == CollectionRole.EDITOR


def test_group_grant():
    acl = [AclRow("group", "g-eng", "viewer")]
    assert effective_collection_role(user(groups=["g-eng"]), acl) == CollectionRole.VIEWER
    assert effective_collection_role(user(groups=["g-other"]), acl) is None


def test_highest_role_wins():
    acl = [AclRow("group", "g-eng", "viewer"), AclRow("user", "u1", "owner")]
    assert effective_collection_role(user(groups=["g-eng"]), acl) == CollectionRole.OWNER


def test_admin_is_implicit_owner():
    assert effective_collection_role(user(roles=["admin"]), []) == CollectionRole.OWNER


def test_public_collection_grants_viewer_to_everyone():
    assert effective_collection_role(user(oid="stranger"), [], public=True) == (
        CollectionRole.VIEWER
    )
    assert effective_collection_role(user(oid="stranger"), [], public=False) is None
    # An explicit grant still outranks the implicit public viewer.
    acl = [AclRow("user", "u1", "editor")]
    assert effective_collection_role(user(), acl, public=True) == CollectionRole.EDITOR


def test_require_role_enforces_hierarchy():
    acl = [AclRow("user", "u1", "viewer")]
    require_collection_role(user(), acl, CollectionRole.VIEWER)
    with pytest.raises(AuthorizationError):
        require_collection_role(user(), acl, CollectionRole.EDITOR)
    with pytest.raises(AuthorizationError):
        require_collection_role(user(oid="stranger"), acl, CollectionRole.VIEWER)
    # Public visibility caps out at viewer.
    require_collection_role(user(oid="stranger"), acl, CollectionRole.VIEWER, public=True)
    with pytest.raises(AuthorizationError):
        require_collection_role(user(oid="stranger"), acl, CollectionRole.EDITOR, public=True)


def test_collection_creation_rights():
    assert can_create_collections(user(roles=["user"]))
    assert can_create_collections(user(roles=["admin"]))
    assert not can_create_collections(user(roles=[]))
    # API-key principals can never create collections.
    key_principal = user(roles=["user"], auth_method="api_key", key_scopes=set())
    assert not can_create_collections(key_principal)
