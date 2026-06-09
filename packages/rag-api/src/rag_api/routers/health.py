from __future__ import annotations

from fastapi import APIRouter, Response, status
from sqlalchemy import text

from rag_core.db.session import get_session_factory

router = APIRouter(tags=["health"])


@router.get("/healthz")
async def liveness() -> dict:
    return {"status": "ok"}


@router.get("/readyz")
async def readiness(response: Response) -> dict:
    try:
        async with get_session_factory()() as session:
            await session.execute(text("SELECT 1"))
    except Exception as exc:
        response.status_code = status.HTTP_503_SERVICE_UNAVAILABLE
        return {"status": "degraded", "database": str(exc)}
    return {"status": "ok", "database": "up"}
