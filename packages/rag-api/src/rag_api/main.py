from __future__ import annotations

from contextlib import asynccontextmanager

import structlog
from fastapi import FastAPI
from fastapi.staticfiles import StaticFiles

from rag_api.routers import (
    analytics,
    apikeys,
    collections,
    health,
    plugins,
    search,
    sources,
    syncs,
    ui,
    uploads,
)
from rag_core.config import get_settings
from rag_core.logging import configure_logging
from rag_core.queue import get_app as get_queue_app

log = structlog.get_logger(__name__)


@asynccontextmanager
async def lifespan(app: FastAPI):
    settings = get_settings()
    configure_logging(settings.log_level)
    # Open the procrastinate connection pool so manual sync triggers can defer jobs.
    queue_app = get_queue_app()
    async with queue_app.open_async():
        log.info("api.started", environment=settings.environment, auth_mode=settings.auth.mode)
        yield
    from rag_core.db.session import get_engine

    await get_engine().dispose()


def create_app() -> FastAPI:
    app = FastAPI(
        title="RAG Platform API",
        version="0.1.0",
        lifespan=lifespan,
        # OpenAPI security hint for Swagger "Authorize" button.
        swagger_ui_init_oauth=None,
    )
    app.include_router(health.router)
    app.include_router(collections.router)
    app.include_router(sources.router)
    app.include_router(uploads.router)
    app.include_router(syncs.router)
    app.include_router(search.router)
    app.include_router(plugins.router)
    app.include_router(apikeys.router)
    app.include_router(analytics.router)
    app.include_router(ui.router)
    app.mount("/ui", StaticFiles(directory=ui.STATIC_DIR, html=True), name="ui")
    return app


app = create_app()
