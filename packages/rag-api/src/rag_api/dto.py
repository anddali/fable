"""Request/response models for the HTTP API."""

from __future__ import annotations

import uuid
from datetime import datetime
from typing import Any, Literal

from pydantic import BaseModel, Field

from rag_core.schemas import PipelineConfig, SearchHit

# --- Collections -----------------------------------------------------------


class CollectionCreate(BaseModel):
    name: str = Field(min_length=1, max_length=200)
    description: str = ""
    visibility: Literal["private", "public"] = "private"
    pipeline: PipelineConfig = Field(default_factory=PipelineConfig)
    sync_cron: str = "0 2 * * *"
    sync_enabled: bool = True


class CollectionUpdate(BaseModel):
    description: str | None = None
    visibility: Literal["private", "public"] | None = None
    pipeline: PipelineConfig | None = None
    sync_cron: str | None = None
    sync_enabled: bool | None = None


class CollectionOut(BaseModel):
    id: uuid.UUID
    name: str
    description: str
    visibility: str
    pipeline: dict[str, Any]
    sync_cron: str
    sync_enabled: bool
    next_sync_at: datetime | None
    created_by: str
    created_at: datetime
    my_role: str | None = None

    model_config = {"from_attributes": True}


class AclEntryIn(BaseModel):
    principal_type: Literal["user", "group"]
    principal_id: str = Field(min_length=1, max_length=100)
    role: Literal["owner", "editor", "viewer"]


class AclEntryOut(AclEntryIn):
    id: uuid.UUID

    model_config = {"from_attributes": True}


# --- Sources ----------------------------------------------------------------


class SourceCreate(BaseModel):
    name: str = Field(min_length=1, max_length=200)
    connector: str
    config: dict[str, Any] = Field(default_factory=dict)
    enabled: bool = True
    # Optional pasted URL resolved into ``config`` by the connector's URL parser
    # (e.g. a Confluence page/space link). Merged over ``config``.
    link: str | None = None
    include_children: bool = False


class SourceUpdate(BaseModel):
    name: str | None = None
    config: dict[str, Any] | None = None
    enabled: bool | None = None


class SourceTestRequest(BaseModel):
    """Dry-run a connector config (or a pasted link) before creating a source."""

    connector: str
    config: dict[str, Any] = Field(default_factory=dict)
    # Optional URL to resolve into config via the connector's URL parser.
    link: str | None = None
    # For link resolution: include descendant pages (page -> tree).
    include_children: bool = False


class SourceTestResult(BaseModel):
    ok: bool
    # Config the connector resolved (e.g. targets derived from the link).
    config: dict[str, Any] = Field(default_factory=dict)
    # A human-readable confirmation (e.g. a sample document title) or the error.
    detail: str = ""


class SourceOut(BaseModel):
    id: uuid.UUID
    collection_id: uuid.UUID
    name: str
    connector: str
    config: dict[str, Any]
    enabled: bool
    last_synced_at: datetime | None
    created_at: datetime

    model_config = {"from_attributes": True}


# --- Uploads / documents ----------------------------------------------------


class UploadItemOut(BaseModel):
    id: uuid.UUID
    filename: str
    mime_type: str
    size_bytes: int
    uploaded_by: str
    created_at: datetime

    model_config = {"from_attributes": True}


class DocumentOut(BaseModel):
    id: uuid.UUID
    source_id: uuid.UUID
    external_id: str
    title: str
    uri: str
    mime_type: str
    status: str
    error: str
    indexed_at: datetime | None
    updated_at: datetime

    model_config = {"from_attributes": True}


class DocumentWithChunksOut(DocumentOut):
    size_bytes: int = 0
    chunk_count: int = 0


class DocumentDetailOut(DocumentWithChunksOut):
    doc_metadata: dict[str, Any] = Field(default_factory=dict)
    content_hash: str = ""
    blob_key: str = ""


class DocumentListOut(BaseModel):
    items: list[DocumentWithChunksOut]
    total: int


class ChunkOut(BaseModel):
    id: uuid.UUID
    ordinal: int
    token_count: int
    text: str
    chunk_metadata: dict[str, Any] = Field(default_factory=dict)
    # The vector itself is never shipped; this just signals indexing health.
    has_embedding: bool = False


class ChunkListOut(BaseModel):
    items: list[ChunkOut]
    total: int


# --- Sync -------------------------------------------------------------------


class SyncRunOut(BaseModel):
    id: uuid.UUID
    collection_id: uuid.UUID
    trigger: str
    status: str
    started_at: datetime
    finished_at: datetime | None
    stats: dict[str, Any]
    error: str

    model_config = {"from_attributes": True}


# --- API keys ----------------------------------------------------------------


class ApiKeyCreate(BaseModel):
    name: str = Field(min_length=1, max_length=200)
    # Collections this key may search; must be readable by the creator.
    collection_ids: list[uuid.UUID] = Field(min_length=1)
    expires_in_days: int | None = Field(default=None, ge=1, le=730)


class ApiKeyOut(BaseModel):
    id: uuid.UUID
    name: str
    prefix: str
    owner_oid: str
    owner_name: str
    scopes: list[str]
    revoked: bool
    expires_at: datetime | None
    last_used_at: datetime | None
    created_at: datetime

    model_config = {"from_attributes": True}


class ApiKeyCreated(ApiKeyOut):
    # Full secret — returned exactly once, never retrievable again.
    secret: str


# --- Search -----------------------------------------------------------------


class SearchRequest(BaseModel):
    query: str = Field(min_length=1, max_length=4000)
    # Empty list = all collections the caller can view.
    collection_ids: list[uuid.UUID] = Field(default_factory=list)
    top_k: int = Field(default=8, ge=1, le=50)
    mode: Literal["vector", "hybrid"] = "hybrid"


class SearchResponse(BaseModel):
    hits: list[SearchHit]
    searched_collection_ids: list[uuid.UUID]


class SearchableCollection(BaseModel):
    """Slim collection view for search clients (API keys / MCP) — no management data."""

    id: uuid.UUID
    name: str
    description: str
    visibility: str

    model_config = {"from_attributes": True}


# --- Chat -------------------------------------------------------------------


class ChatTurn(BaseModel):
    role: Literal["user", "assistant"]
    content: str = Field(min_length=1, max_length=8000)


class ChatRequest(BaseModel):
    query: str = Field(min_length=1, max_length=4000)
    # Prior conversation turns for context; the server keeps only the most
    # recent few (see RAG_CHAT__HISTORY_TURNS).
    history: list[ChatTurn] = Field(default_factory=list)
    # Empty list = every collection the caller can read.
    collection_ids: list[uuid.UUID] = Field(default_factory=list)


class ChatResponse(BaseModel):
    answer: str
    # The retrieved chunks used to ground the answer (the citations).
    hits: list[SearchHit]
    searched_collection_ids: list[uuid.UUID]
