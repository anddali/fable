"""Confluence Cloud connector (REST API v2).

Targets: a whole space, a single page, or a page tree (page + all descendants).
Folder targets are treated as trees (Confluence folders are content nodes whose
children are enumerated the same way).
"""

from __future__ import annotations

import os
import re
from collections.abc import AsyncIterator
from datetime import datetime
from typing import TYPE_CHECKING, Any, ClassVar, Literal
from urllib.parse import urlparse

import httpx
from pydantic import BaseModel, Field

from rag_core.config import ConfluenceConnection
from rag_core.interfaces import ConnectorPlugin, SyncContext
from rag_core.schemas import RawDocument

if TYPE_CHECKING:
    from rag_core.config import Settings

# Confluence Cloud URL shapes (path after the site, e.g. .../wiki/...).
_PAGE_RE = re.compile(r"/pages/(\d+)")
_FOLDER_RE = re.compile(r"/folder/(\d+)")
_SPACE_RE = re.compile(r"/spaces/([^/]+)")


class ConfluenceTarget(BaseModel):
    kind: Literal["space", "page", "tree", "folder"]
    # Space key for kind=space; numeric content id for page/tree/folder.
    id: str


class ConfluenceConfig(BaseModel):
    targets: list[ConfluenceTarget] = Field(min_length=1)
    page_limit: int = 50
    # Which platform connection (service account) to pull through. "default"
    # unless the operator has configured extra named connections.
    connection: str = "default"
    # Deprecated per-source connection fields. Kept so sources created before the
    # global connection existed keep working with no migration: when ``base_url``
    # is set it wins over the platform connection. New sources leave these unset.
    base_url: str | None = None
    user_email: str | None = None
    api_token_env: str | None = None


class ConfluenceConnector(ConnectorPlugin):
    name: ClassVar[str] = "confluence"
    config_model: ClassVar[type[BaseModel]] = ConfluenceConfig
    supports_url_targets: ClassVar[bool] = True

    @classmethod
    def ready(cls, settings: Settings) -> bool:
        return settings.confluence.connection("default").configured

    @classmethod
    def parse_target_url(cls, url: str, settings: Settings) -> dict[str, Any] | None:
        """Parse a Confluence Cloud URL into a ``{"targets": [...]}`` fragment.

        A ``/pages/`` URL yields ``kind="page"`` (a single page); callers flip it
        to ``tree`` when the user asks to include descendants. Returns None for
        URLs this connector can't interpret.
        """
        path = urlparse(url).path
        if m := _PAGE_RE.search(path):
            target = {"kind": "page", "id": m.group(1)}
        elif m := _FOLDER_RE.search(path):
            target = {"kind": "folder", "id": m.group(1)}
        elif m := _SPACE_RE.search(path):
            target = {"kind": "space", "id": m.group(1)}
        else:
            return None
        return {"targets": [target]}

    def _connection(self, settings: Settings) -> ConfluenceConnection:
        cfg: ConfluenceConfig = self.config  # type: ignore[assignment]
        if cfg.base_url:  # legacy per-source config wins for back-compat
            return ConfluenceConnection(
                base_url=cfg.base_url,
                user_email=cfg.user_email or "",
                api_token_env=cfg.api_token_env or "CONFLUENCE_API_TOKEN",
            )
        return settings.confluence.connection(cfg.connection)

    async def iter_documents(self, ctx: SyncContext) -> AsyncIterator[RawDocument]:
        cfg: ConfluenceConfig = self.config  # type: ignore[assignment]
        conn = self._connection(ctx.settings)
        if not conn.configured:
            raise RuntimeError(
                f"Confluence connection {cfg.connection!r} is not configured "
                "(set RAG_CONFLUENCE__BASE_URL and RAG_CONFLUENCE__USER_EMAIL)"
            )
        token = os.environ.get(conn.api_token_env, "")
        if not token:
            raise RuntimeError(f"Environment variable {conn.api_token_env!r} is not set")

        seen: set[str] = set()
        async with httpx.AsyncClient(
            base_url=conn.base_url.rstrip("/"),
            auth=(conn.user_email, token),
            timeout=60,
            follow_redirects=True,
        ) as client:
            for target in cfg.targets:
                if target.kind == "space":
                    page_ids = self._space_pages(client, cfg, target.id)
                elif target.kind == "page":
                    page_ids = _aiter_single(target.id)
                else:  # tree / folder
                    page_ids = self._tree_pages(client, cfg, target.id)

                async for page_id in page_ids:
                    if page_id in seen:
                        continue
                    seen.add(page_id)
                    doc = await self._fetch_page(client, conn.base_url, page_id)
                    if doc is not None:
                        yield doc

    async def _space_pages(
        self, client: httpx.AsyncClient, cfg: ConfluenceConfig, space_key: str
    ) -> AsyncIterator[str]:
        space_id = await self._resolve_space_id(client, space_key)
        url = f"/api/v2/spaces/{space_id}/pages"
        params: dict = {"limit": cfg.page_limit, "status": "current"}
        async for item in self._paginate(client, url, params):
            yield str(item["id"])

    async def _tree_pages(
        self, client: httpx.AsyncClient, cfg: ConfluenceConfig, root_id: str
    ) -> AsyncIterator[str]:
        yield root_id
        stack = [root_id]
        while stack:
            parent = stack.pop()
            url = f"/api/v2/pages/{parent}/children"
            async for child in self._paginate(client, url, {"limit": cfg.page_limit}):
                child_id = str(child["id"])
                stack.append(child_id)
                yield child_id

    async def _resolve_space_id(self, client: httpx.AsyncClient, space_key: str) -> str:
        if space_key.isdigit():
            return space_key
        resp = await client.get("/api/v2/spaces", params={"keys": space_key, "limit": 1})
        resp.raise_for_status()
        results = resp.json().get("results", [])
        if not results:
            raise RuntimeError(f"Confluence space {space_key!r} not found")
        return str(results[0]["id"])

    async def _paginate(
        self, client: httpx.AsyncClient, url: str, params: dict
    ) -> AsyncIterator[dict]:
        next_url: str | None = url
        next_params: dict | None = params
        while next_url:
            resp = await client.get(next_url, params=next_params)
            resp.raise_for_status()
            body = resp.json()
            for item in body.get("results", []):
                yield item
            next_url = (body.get("_links") or {}).get("next")
            next_params = None  # the "next" link already carries the cursor

    async def _fetch_page(
        self, client: httpx.AsyncClient, base_url: str, page_id: str
    ) -> RawDocument | None:
        resp = await client.get(
            f"/api/v2/pages/{page_id}",
            params={"body-format": "storage", "include-version": "true"},
        )
        if resp.status_code == 404:
            return None
        resp.raise_for_status()
        page = resp.json()
        body_html = ((page.get("body") or {}).get("storage") or {}).get("value", "")
        version = page.get("version") or {}
        web_ui = ((page.get("_links") or {}).get("webui")) or f"/pages/{page_id}"
        updated_at = None
        if created := version.get("createdAt"):
            updated_at = datetime.fromisoformat(created.replace("Z", "+00:00"))
        return RawDocument(
            external_id=f"page:{page_id}",
            title=page.get("title", ""),
            uri=f"{base_url.rstrip('/')}{web_ui}",
            mime_type="text/html",
            text=body_html,
            source_updated_at=updated_at,
            metadata={
                "space_id": str(page.get("spaceId", "")),
                "version": version.get("number"),
            },
        )


async def _aiter_single(value: str) -> AsyncIterator[str]:
    yield value
