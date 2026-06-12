from __future__ import annotations

from contextlib import asynccontextmanager

import structlog
from fastapi import Depends, FastAPI
from fastapi.staticfiles import StaticFiles
from starlette.responses import Response
from fastapi_mcp import AuthConfig, FastApiMCP

from rag_api.auth import get_principal
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


class NoCacheStaticFiles(StaticFiles):
    """Static files that browsers must revalidate on every load.

    Without Cache-Control browsers cache app.js/app.css heuristically, so UI
    deploys appear stale until a hard refresh. no-cache still permits 304
    responses via the ETag/Last-Modified that StaticFiles already sends.
    """

    def file_response(self, *args, **kwargs) -> Response:
        response = super().file_response(*args, **kwargs)
        response.headers["Cache-Control"] = "no-cache"
        return response


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
    app.mount("/ui", NoCacheStaticFiles(directory=ui.STATIC_DIR, html=True), name="ui")

    # MCP server at /mcp: exposes search + collection discovery as tools. The
    # client's Authorization header is forwarded to the endpoint, so API keys
    # and Entra tokens work exactly as they do over REST (keys stay search-only).
    mcp = FastApiMCP(
        app,
        name="RAG Platform",
        description="Semantic search over indexed document collections",
        include_operations=["search_documents", "list_searchable_collections"],
        auth_config=AuthConfig(dependencies=[Depends(get_principal)]),
    )
    mcp.mount_http()
    return app


app = create_app()
