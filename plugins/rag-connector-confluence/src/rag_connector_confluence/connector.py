"""Confluence Cloud connector (REST API v2).

Targets: a whole space, a single page, or a page tree (page + all descendants).
Folder targets are treated as trees (Confluence folders are content nodes whose
children are enumerated the same way).
"""

from __future__ import annotations

import base64
import html as html_mod
import os
import re
from collections.abc import AsyncIterator
from datetime import datetime
from typing import TYPE_CHECKING, Any, ClassVar, Literal
from urllib.parse import urlparse

import httpx
import structlog
from pydantic import BaseModel, Field

from rag_core.config import ConfluenceConnection
from rag_core.interfaces import ConnectorPlugin, SyncContext
from rag_core.schemas import RawDocument

if TYPE_CHECKING:
    from rag_core.config import Settings

log = structlog.get_logger(__name__)

# Confluence Cloud URL shapes (path after the site, e.g. .../wiki/...).
_PAGE_RE = re.compile(r"/pages/(\d+)")
_FOLDER_RE = re.compile(r"/folder/(\d+)")
_SPACE_RE = re.compile(r"/spaces/([^/]+)")

# Storage-format image macros: <ac:image ...><ri:attachment ri:filename="..."/></ac:image>.
_AC_IMAGE_RE = re.compile(r"<ac:image\b[^>]*>.*?</ac:image>", re.DOTALL)
_RI_ATTACHMENT_RE = re.compile(r'<ri:attachment\b[^>]*\bri:filename="([^"]*)"')

# Raster types the vision pipeline can describe; SVG and other vector or
# non-image attachments are skipped.
_IMAGE_MIMES = {"image/png", "image/jpeg", "image/gif", "image/webp"}
_MAX_IMAGE_BYTES = 20 * 1024 * 1024


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
        body_html = await self._inline_images(client, page_id, body_html)
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

    async def _inline_images(self, client: httpx.AsyncClient, page_id: str, body_html: str) -> str:
        """Replace attachment image macros with data-URI ``<img>`` tags.

        Storage format embeds images as ``<ac:image><ri:attachment
        ri:filename="..."/></ac:image>`` — a reference, not a URL — so the
        bytes must be pulled through the attachments API while we still hold
        the authenticated client. Inlining them into the payload (rather than
        stashing bytes in metadata) keeps images out of the database and makes
        the content hash cover image edits, so delta detection reindexes a
        page whose text is unchanged but whose figure was replaced.

        External images (``<ri:url>``) are left alone: fetching them with this
        client would send the Confluence credentials to an arbitrary host.
        Unresolvable or non-raster attachments are also left as-is; the HTML
        parser drops unknown macros, so failures degrade to today's behavior.
        """
        macros = _AC_IMAGE_RE.findall(body_html)
        wanted = {
            html_mod.unescape(m.group(1))
            for macro in macros
            if (m := _RI_ATTACHMENT_RE.search(macro))
        }
        if not wanted:
            return body_html

        links: dict[str, tuple[str, str]] = {}  # filename -> (download link, mime)
        async for item in self._paginate(
            client, f"/api/v2/pages/{page_id}/attachments", {"limit": 50}
        ):
            title = item.get("title") or ""
            if title in wanted:
                links[title] = (item.get("downloadLink") or "", item.get("mediaType") or "")

        data_uris: dict[str, str] = {}
        for name in wanted:
            link, mime = links.get(name, ("", ""))
            if not link or mime not in _IMAGE_MIMES:
                continue
            try:
                resp = await client.get(link)
                resp.raise_for_status()
            except httpx.HTTPError:
                log.warning("confluence.attachment_fetch_failed", page_id=page_id, filename=name)
                continue
            if len(resp.content) > _MAX_IMAGE_BYTES:
                log.warning("confluence.attachment_too_large", page_id=page_id, filename=name)
                continue
            b64 = base64.b64encode(resp.content).decode("ascii")
            data_uris[name] = f"data:{mime};base64,{b64}"

        def _sub(match: re.Match) -> str:
            if m := _RI_ATTACHMENT_RE.search(match.group(0)):
                name = html_mod.unescape(m.group(1))
                if uri := data_uris.get(name):
                    return f'<img src="{uri}" alt="{html_mod.escape(name)}">'
            return match.group(0)

        return _AC_IMAGE_RE.sub(_sub, body_html)


async def _aiter_single(value: str) -> AsyncIterator[str]:
    yield value
