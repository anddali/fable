"""Management UI: static SPA + unauthenticated bootstrap config.

The SPA authenticates in the browser with MSAL (authorization code + PKCE)
against the SPA app registration, then calls this API with bearer tokens.
``/ui-config`` is intentionally public: it only exposes identifiers a SPA
needs anyway (tenant + client id), never secrets.
"""

from __future__ import annotations

from pathlib import Path

from fastapi import APIRouter
from fastapi.responses import RedirectResponse

from rag_api.auth import CurrentUser
from rag_core.config import get_settings

router = APIRouter(tags=["ui"])

STATIC_DIR = Path(__file__).resolve().parent.parent / "static"


@router.get("/ui-config")
async def ui_config() -> dict:
    settings = get_settings()
    return {
        "auth_mode": settings.auth.mode,
        "tenant_id": settings.auth.tenant_id,
        "spa_client_id": settings.auth.spa_client_id,
        # Delegated scope the SPA requests for this API.
        "api_scope": settings.auth.api_scope
        or (f"{settings.auth.audience}/.default" if settings.auth.audience else ""),
        "environment": settings.environment,
    }


@router.get("/me")
async def me(principal: CurrentUser) -> dict:
    return {
        "oid": principal.oid,
        "name": principal.name,
        "email": principal.email,
        "roles": sorted(principal.global_roles),
        "is_admin": principal.is_admin,
    }


@router.get("/", include_in_schema=False)
async def root() -> RedirectResponse:
    return RedirectResponse(url="/ui/")
