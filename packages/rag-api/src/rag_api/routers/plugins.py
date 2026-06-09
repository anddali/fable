from __future__ import annotations

from fastapi import APIRouter

from rag_api.auth import require_global
from rag_core import plugins

router = APIRouter(prefix="/plugins", tags=["plugins"])


@router.get("", dependencies=[require_global("curator")])
async def list_plugins() -> dict:
    """All installed plugins per kind, with their JSON config schemas."""
    return plugins.describe_all()
