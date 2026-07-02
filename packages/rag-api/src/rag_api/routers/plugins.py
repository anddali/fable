from __future__ import annotations

from fastapi import APIRouter

from rag_api.auth import require_global
from rag_core import plugins
from rag_core.config import get_settings

router = APIRouter(prefix="/plugins", tags=["plugins"])


@router.get("", dependencies=[require_global("user")])
async def list_plugins() -> dict:
    """All installed plugins per kind, with their JSON config schemas.

    Connector entries are annotated with ``ready`` (usable given platform config,
    e.g. a configured service-account connection) and ``supports_url_targets``
    (the UI can offer a paste-a-link field instead of raw JSON).
    """
    data = plugins.describe_all()
    settings = get_settings()
    for entry in data.get("connector", []):
        cls = plugins.get_class("connector", entry["name"])
        entry["ready"] = cls.ready(settings)  # type: ignore[attr-defined]
        entry["supports_url_targets"] = cls.supports_url_targets  # type: ignore[attr-defined]
    return data
