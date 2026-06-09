"""SharePoint connector: walks a document library (drive) via Microsoft Graph.

Auth: client-credentials flow against Entra ID with the Graph application
permission ``Sites.Read.All`` (or ``Sites.Selected`` granted per site).
"""

from __future__ import annotations

import os
from collections.abc import AsyncIterator
from datetime import datetime
from typing import ClassVar

import httpx
from pydantic import BaseModel, Field

from rag_core.interfaces import ConnectorPlugin, SyncContext
from rag_core.schemas import RawDocument

GRAPH = "https://graph.microsoft.com/v1.0"

_EXT_MIME = {
    ".pdf": "application/pdf",
    ".txt": "text/plain",
    ".md": "text/markdown",
    ".csv": "text/csv",
    ".xlsx": "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
}


class SharePointConfig(BaseModel):
    tenant_id: str
    client_id: str
    # Environment variable holding the client secret.
    client_secret_env: str = "SHAREPOINT_CLIENT_SECRET"
    # Graph drive id of the document library (GET /sites/{site-id}/drives).
    drive_id: str
    # Optional folder path inside the drive ("" = library root).
    root_path: str = ""
    # Only files with these extensions are ingested (must have a parser).
    extensions: list[str] = Field(default_factory=lambda: sorted(_EXT_MIME))
    max_file_bytes: int = 50 * 1024 * 1024


class SharePointConnector(ConnectorPlugin):
    name: ClassVar[str] = "sharepoint"
    config_model: ClassVar[type[BaseModel]] = SharePointConfig

    async def iter_documents(self, ctx: SyncContext) -> AsyncIterator[RawDocument]:
        cfg: SharePointConfig = self.config  # type: ignore[assignment]
        secret = os.environ.get(cfg.client_secret_env, "")
        if not secret:
            raise RuntimeError(f"Environment variable {cfg.client_secret_env!r} is not set")

        async with httpx.AsyncClient(timeout=120) as client:
            token = await self._get_token(client, cfg, secret)
            headers = {"Authorization": f"Bearer {token}"}

            root = (
                f"{GRAPH}/drives/{cfg.drive_id}/root:/{cfg.root_path.strip('/')}:"
                if cfg.root_path.strip("/")
                else f"{GRAPH}/drives/{cfg.drive_id}/root"
            )
            stack = [f"{root}/children"]
            allowed = {e.lower() for e in cfg.extensions}

            while stack:
                url = stack.pop()
                while url:
                    resp = await client.get(url, headers=headers)
                    resp.raise_for_status()
                    body = resp.json()
                    for item in body.get("value", []):
                        if "folder" in item:
                            stack.append(
                                f"{GRAPH}/drives/{cfg.drive_id}/items/{item['id']}/children"
                            )
                            continue
                        if "file" not in item:
                            continue
                        ext = (
                            "." + item["name"].rsplit(".", 1)[-1].lower()
                            if "." in item["name"]
                            else ""
                        )
                        if ext not in allowed or item.get("size", 0) > cfg.max_file_bytes:
                            continue
                        doc = await self._fetch_file(client, headers, cfg, item, ext)
                        if doc is not None:
                            yield doc
                    url = body.get("@odata.nextLink")

    async def _get_token(
        self, client: httpx.AsyncClient, cfg: SharePointConfig, secret: str
    ) -> str:
        resp = await client.post(
            f"https://login.microsoftonline.com/{cfg.tenant_id}/oauth2/v2.0/token",
            data={
                "grant_type": "client_credentials",
                "client_id": cfg.client_id,
                "client_secret": secret,
                "scope": "https://graph.microsoft.com/.default",
            },
        )
        resp.raise_for_status()
        return resp.json()["access_token"]

    async def _fetch_file(
        self,
        client: httpx.AsyncClient,
        headers: dict,
        cfg: SharePointConfig,
        item: dict,
        ext: str,
    ) -> RawDocument | None:
        resp = await client.get(
            f"{GRAPH}/drives/{cfg.drive_id}/items/{item['id']}/content",
            headers=headers,
            follow_redirects=True,
        )
        if resp.status_code == 404:
            return None
        resp.raise_for_status()
        modified = item.get("lastModifiedDateTime")
        # Graph's quickXorHash/cTag identifies content versions without
        # downloading; we still hash bytes ourselves for cross-source parity,
        # but cTag short-circuits within the metadata.
        return RawDocument(
            external_id=f"driveItem:{item['id']}",
            title=item.get("name", ""),
            uri=item.get("webUrl", ""),
            mime_type=_EXT_MIME.get(ext, "application/octet-stream"),
            content=resp.content,
            source_updated_at=(
                datetime.fromisoformat(modified.replace("Z", "+00:00")) if modified else None
            ),
            metadata={
                "path": ((item.get("parentReference") or {}).get("path", "")),
                "ctag": item.get("cTag", ""),
            },
        )
