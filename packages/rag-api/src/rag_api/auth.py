"""Microsoft Entra ID bearer-token validation (OAuth2 resource server).

Clients (SPA, CLI, service) obtain access tokens from Entra ID via the
authorization-code + PKCE or client-credentials flow; this API only validates
them. Global roles come from Entra *app roles* (``roles`` claim), group-based
collection access from the ``groups`` claim (enable group claims on the app
registration, or resolve via Graph for overage cases).
"""

from __future__ import annotations

from functools import lru_cache
from typing import Annotated

import jwt
import structlog
from fastapi import Depends, HTTPException, Request, status
from fastapi.security import HTTPAuthorizationCredentials, HTTPBearer

from rag_core.config import Settings, get_settings
from rag_core.rbac import map_global_roles
from rag_core.schemas import Principal

log = structlog.get_logger(__name__)

_bearer = HTTPBearer(auto_error=False)

DEV_PRINCIPAL = Principal(
    oid="00000000-0000-0000-0000-000000000001",
    name="Local Developer",
    email="dev@localhost",
    global_roles={"admin", "curator", "reader"},
)


@lru_cache
def _jwk_client(jwks_uri: str) -> jwt.PyJWKClient:
    # PyJWKClient caches keys internally; lru_cache keeps one client per URI.
    return jwt.PyJWKClient(jwks_uri, cache_keys=True, lifespan=3600)


def _unauthorized(detail: str) -> HTTPException:
    return HTTPException(
        status_code=status.HTTP_401_UNAUTHORIZED,
        detail=detail,
        headers={"WWW-Authenticate": "Bearer"},
    )


def decode_token(token: str, settings: Settings) -> Principal:
    auth = settings.auth
    try:
        signing_key = _jwk_client(auth.jwks_uri).get_signing_key_from_jwt(token)
        claims = jwt.decode(
            token,
            signing_key.key,
            algorithms=["RS256"],
            audience=auth.audience,
            issuer=auth.issuer,
            options={"require": ["exp", "iss", "aud", "sub"]},
        )
    except jwt.PyJWTError as exc:
        log.info("auth.token_rejected", reason=str(exc))
        raise _unauthorized("Invalid or expired token") from exc

    oid = claims.get("oid") or claims.get("sub")
    return Principal(
        oid=oid,
        name=claims.get("name", ""),
        email=claims.get("preferred_username") or claims.get("upn", ""),
        global_roles=map_global_roles(claims.get("roles", []), auth.role_map),
        group_ids=set(claims.get("groups", [])),
    )


async def get_principal(
    request: Request,
    credentials: Annotated[HTTPAuthorizationCredentials | None, Depends(_bearer)],
    settings: Annotated[Settings, Depends(get_settings)],
) -> Principal:
    if settings.auth.mode == "disabled":
        if settings.environment == "prod":
            raise RuntimeError("auth.mode=disabled is not permitted in prod")
        return DEV_PRINCIPAL
    if credentials is None:
        raise _unauthorized("Missing bearer token")
    principal = decode_token(credentials.credentials, settings)
    if not principal.global_roles:
        # Authenticated but no app role assigned -> no API access at all.
        raise HTTPException(
            status_code=status.HTTP_403_FORBIDDEN,
            detail="No platform role assigned; ask an admin for an Entra app role",
        )
    structlog.contextvars.bind_contextvars(principal=principal.oid)
    return principal


CurrentPrincipal = Annotated[Principal, Depends(get_principal)]


def require_global(role: str):
    """Router dependency: require a platform-global role (admin implies all)."""

    async def _check(principal: CurrentPrincipal) -> Principal:
        if role not in principal.global_roles and not principal.is_admin:
            raise HTTPException(
                status_code=status.HTTP_403_FORBIDDEN,
                detail=f"Requires global role {role!r}",
            )
        return principal

    return Depends(_check)
