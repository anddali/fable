"""Data contracts shared between connectors, the indexing pipeline and the API."""

from __future__ import annotations

import hashlib
from datetime import datetime
from typing import Any, Literal
from uuid import UUID

from pydantic import BaseModel, Field

# ---------------------------------------------------------------------------
# Identity / RBAC
# ---------------------------------------------------------------------------


class Principal(BaseModel):
    """Authenticated caller. ``oid`` and ``group_ids`` are Entra object ids.

    API-key callers get ``auth_method="api_key"``, the owning user's oid, no
    group claims, and ``key_scopes`` (collection ids the key may search).
    """

    oid: str
    name: str = ""
    email: str = ""
    global_roles: set[str] = Field(default_factory=set)  # admin | user
    group_ids: set[str] = Field(default_factory=set)
    auth_method: Literal["sso", "api_key"] = "sso"
    api_key_id: str | None = None
    key_scopes: set[str] | None = None  # collection ids (str UUIDs)

    @property
    def is_admin(self) -> bool:
        return "admin" in self.global_roles


# ---------------------------------------------------------------------------
# Connector output
# ---------------------------------------------------------------------------


class RawDocument(BaseModel):
    """One item emitted by a connector during sync.

    Connectors provide either ``content`` (bytes, e.g. a PDF) or ``text``
    (already-decoded text, e.g. Slack messages). ``content_hash`` drives delta
    detection: an unchanged hash skips re-indexing entirely.
    """

    external_id: str
    title: str = ""
    uri: str = ""
    mime_type: str = "text/plain"
    content: bytes | None = None
    text: str | None = None
    metadata: dict[str, Any] = Field(default_factory=dict)
    content_hash: str = ""
    source_updated_at: datetime | None = None

    def payload(self) -> bytes:
        if self.content is not None:
            return self.content
        if self.text is not None:
            return self.text.encode("utf-8")
        raise ValueError(f"RawDocument {self.external_id!r} has neither content nor text")

    def ensure_hash(self) -> str:
        if not self.content_hash:
            self.content_hash = hashlib.sha256(self.payload()).hexdigest()
        return self.content_hash


# ---------------------------------------------------------------------------
# Parsed representation
# ---------------------------------------------------------------------------

BlockType = Literal["heading", "paragraph", "code", "table_row", "list_item"]


class Block(BaseModel):
    type: BlockType = "paragraph"
    text: str
    level: int = 0  # heading level for type == "heading"
    metadata: dict[str, Any] = Field(default_factory=dict)


class ParsedDocument(BaseModel):
    title: str = ""
    blocks: list[Block] = Field(default_factory=list)
    metadata: dict[str, Any] = Field(default_factory=dict)


class ChunkDraft(BaseModel):
    ordinal: int
    text: str
    token_count: int
    metadata: dict[str, Any] = Field(default_factory=dict)


# ---------------------------------------------------------------------------
# Per-collection pipeline configuration (stored as JSONB on the collection)
# ---------------------------------------------------------------------------


class PluginRef(BaseModel):
    name: str
    config: dict[str, Any] = Field(default_factory=dict)


class PipelineConfig(BaseModel):
    """Which plugins a collection uses for indexing.

    ``parser`` is optional: when unset, the parser is auto-selected from the
    document mime type. The embedder is platform-global (see EmbeddingSettings)
    and intentionally not configurable per collection.
    """

    parser: PluginRef | None = None
    transformers: list[PluginRef] = Field(default_factory=lambda: [PluginRef(name="basic-cleanup")])
    chunker: PluginRef = Field(default_factory=lambda: PluginRef(name="default"))


# ---------------------------------------------------------------------------
# Retrieval
# ---------------------------------------------------------------------------


class SearchHit(BaseModel):
    chunk_id: UUID
    document_id: UUID
    collection_id: UUID
    score: float
    text: str
    title: str = ""
    uri: str = ""
    ordinal: int = 0
    metadata: dict[str, Any] = Field(default_factory=dict)
