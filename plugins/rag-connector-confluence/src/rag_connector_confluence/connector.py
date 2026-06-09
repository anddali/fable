"""Confluence Cloud connector (REST API v2).

Targets: a whole space, a single page, or a page tree (page + all descendants).
Folder targets are treated as trees (Confluence folders are content nodes whose
children are enumerated the same way).
"""

from __future__ import annotations

import os
from collections.abc import AsyncIterator
from datetime import datetime
from typing import ClassVar, Literal

import httpx
from pydantic import BaseModel, Field

from rag_core.interfaces import ConnectorPlugin, SyncContext
from rag_core.schemas import RawDocument


class ConfluenceTarget(BaseModel):
    kind: Literal["space", "page", "tree", "folder"]
    # Space key for kind=space; numeric content id for page/tree/folder.
    id: str


class ConfluenceConfig(BaseModel):
    # e.g. https://yourorg.atlassian.net/wiki
    base_url: str
    user_email: str
    # Environment variable holding the Atlassian API token (never the token itself).
    api_token_env: str = "CONFLUENCE_API_TOKEN"
    targets: list[ConfluenceTarget] = Field(min_length=1)
    page_limit: int = 50


class ConfluenceConnector(ConnectorPlugin):
    name: ClassVar[str] = "confluence"
    config_model: ClassVar[type[BaseModel]] = ConfluenceConfig

    async def iter_documents(self, ctx: SyncContext) -> AsyncIterator[RawDocument]:
        cfg: ConfluenceConfig = self.config  # type: ignore[assignment]
        token = os.environ.get(cfg.api_token_env, "")
        if not token:
            raise RuntimeError(f"Environment variable {cfg.api_token_env!r} is not set")

        seen: set[str] = set()
        async with httpx.AsyncClient(
            base_url=cfg.base_url.rstrip("/"),
            auth=(cfg.user_email, token),
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
                    doc = await self._fetch_page(client, cfg, page_id)
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
        self, client: httpx.AsyncClient, cfg: ConfluenceConfig, page_id: str
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
            uri=f"{cfg.base_url.rstrip('/')}{web_ui}",
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
