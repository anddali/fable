"""Relational model.

collections ──< sources ──< documents ──< chunks
     │              └──< upload_items (manual-upload staging)
     ├──< collection_principals (ACL)
     └──< sync_runs
"""

from __future__ import annotations

import uuid
from datetime import datetime
from typing import Any

from pgvector.sqlalchemy import Vector
from sqlalchemy import (
    BigInteger,
    Boolean,
    DateTime,
    ForeignKey,
    Index,
    Integer,
    String,
    Text,
    UniqueConstraint,
)
from sqlalchemy.dialects.postgresql import JSONB
from sqlalchemy.dialects.postgresql import UUID as PGUUID
from sqlalchemy.orm import Mapped, mapped_column, relationship

from rag_core.db.base import Base, TimestampMixin, UUIDPKMixin

# Status / role values are plain strings with app-level validation: cheaper to
# evolve than Postgres enums.
DOCUMENT_STATUSES = ("pending", "indexed", "failed", "deleted")
SYNC_STATUSES = ("running", "succeeded", "failed")
COLLECTION_ROLES = ("owner", "editor", "viewer")
PRINCIPAL_TYPES = ("user", "group")


class Collection(UUIDPKMixin, TimestampMixin, Base):
    __tablename__ = "collections"

    name: Mapped[str] = mapped_column(String(200), unique=True, nullable=False)
    description: Mapped[str] = mapped_column(Text, default="", nullable=False)
    # PipelineConfig (parser/transformers/chunker plugin refs), see rag_core.schemas.
    pipeline: Mapped[dict[str, Any]] = mapped_column(JSONB, default=dict, nullable=False)
    # Standard 5-field cron expression evaluated by the worker beat.
    sync_cron: Mapped[str] = mapped_column(String(100), default="0 2 * * *", nullable=False)
    sync_enabled: Mapped[bool] = mapped_column(Boolean, default=True, nullable=False)
    next_sync_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    created_by: Mapped[str] = mapped_column(String(100), default="", nullable=False)

    sources: Mapped[list[Source]] = relationship(back_populates="collection")
    principals: Mapped[list[CollectionPrincipal]] = relationship(
        back_populates="collection", cascade="all, delete-orphan"
    )


class CollectionPrincipal(UUIDPKMixin, TimestampMixin, Base):
    """Collection-level ACL entry: grants a role to an Entra user or group."""

    __tablename__ = "collection_principals"
    __table_args__ = (UniqueConstraint("collection_id", "principal_type", "principal_id"),)

    collection_id: Mapped[uuid.UUID] = mapped_column(
        ForeignKey("collections.id", ondelete="CASCADE"), nullable=False, index=True
    )
    principal_type: Mapped[str] = mapped_column(String(10), nullable=False)  # user | group
    principal_id: Mapped[str] = mapped_column(String(100), nullable=False)  # Entra object id
    role: Mapped[str] = mapped_column(String(10), nullable=False)  # owner | editor | viewer

    collection: Mapped[Collection] = relationship(back_populates="principals")


class Source(UUIDPKMixin, TimestampMixin, Base):
    __tablename__ = "sources"
    __table_args__ = (UniqueConstraint("collection_id", "name"),)

    collection_id: Mapped[uuid.UUID] = mapped_column(
        ForeignKey("collections.id", ondelete="CASCADE"), nullable=False, index=True
    )
    name: Mapped[str] = mapped_column(String(200), nullable=False)
    # Entry-point name of the connector plugin ("confluence", "upload", ...).
    connector: Mapped[str] = mapped_column(String(100), nullable=False)
    # Validated against the connector's config_model. Never store secrets here;
    # store names of environment variables instead.
    config: Mapped[dict[str, Any]] = mapped_column(JSONB, default=dict, nullable=False)
    enabled: Mapped[bool] = mapped_column(Boolean, default=True, nullable=False)
    # Connector-owned incremental cursor state (Slack ts, Graph delta links...).
    state: Mapped[dict[str, Any]] = mapped_column(JSONB, default=dict, nullable=False)
    last_synced_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))

    collection: Mapped[Collection] = relationship(back_populates="sources")
    documents: Mapped[list[Document]] = relationship(back_populates="source")


class UploadItem(UUIDPKMixin, TimestampMixin, Base):
    """Staged manual upload, surfaced to indexing by the 'upload' connector."""

    __tablename__ = "upload_items"

    source_id: Mapped[uuid.UUID] = mapped_column(
        ForeignKey("sources.id", ondelete="CASCADE"), nullable=False, index=True
    )
    filename: Mapped[str] = mapped_column(String(500), nullable=False)
    mime_type: Mapped[str] = mapped_column(String(200), nullable=False)
    size_bytes: Mapped[int] = mapped_column(BigInteger, nullable=False)
    blob_key: Mapped[str] = mapped_column(String(800), nullable=False)
    content_hash: Mapped[str] = mapped_column(String(64), nullable=False)
    uploaded_by: Mapped[str] = mapped_column(String(100), default="", nullable=False)
    deleted: Mapped[bool] = mapped_column(Boolean, default=False, nullable=False)


class Document(UUIDPKMixin, TimestampMixin, Base):
    __tablename__ = "documents"
    __table_args__ = (UniqueConstraint("source_id", "external_id"),)

    source_id: Mapped[uuid.UUID] = mapped_column(
        ForeignKey("sources.id", ondelete="CASCADE"), nullable=False, index=True
    )
    # Denormalized for retrieval-time ACL filtering without an extra join.
    collection_id: Mapped[uuid.UUID] = mapped_column(
        ForeignKey("collections.id", ondelete="CASCADE"), nullable=False, index=True
    )
    external_id: Mapped[str] = mapped_column(String(800), nullable=False)
    title: Mapped[str] = mapped_column(String(1000), default="", nullable=False)
    uri: Mapped[str] = mapped_column(String(2000), default="", nullable=False)
    mime_type: Mapped[str] = mapped_column(String(200), default="text/plain", nullable=False)
    content_hash: Mapped[str] = mapped_column(String(64), default="", nullable=False)
    size_bytes: Mapped[int] = mapped_column(BigInteger, default=0, nullable=False)
    blob_key: Mapped[str] = mapped_column(String(800), default="", nullable=False)
    doc_metadata: Mapped[dict[str, Any]] = mapped_column(JSONB, default=dict, nullable=False)
    status: Mapped[str] = mapped_column(String(10), default="pending", nullable=False)
    error: Mapped[str] = mapped_column(Text, default="", nullable=False)
    source_updated_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    indexed_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))

    source: Mapped[Source] = relationship(back_populates="documents")
    chunks: Mapped[list[Chunk]] = relationship(
        back_populates="document", cascade="all, delete-orphan", passive_deletes=True
    )


class Chunk(UUIDPKMixin, Base):
    __tablename__ = "chunks"
    __table_args__ = (
        UniqueConstraint("document_id", "ordinal"),
        # HNSW + tsvector GIN indexes are created in the Alembic migration
        # (they need raw DDL / a fixed vector dimension).
        Index("ix_chunks_collection_id", "collection_id"),
    )

    document_id: Mapped[uuid.UUID] = mapped_column(
        ForeignKey("documents.id", ondelete="CASCADE"), nullable=False, index=True
    )
    collection_id: Mapped[uuid.UUID] = mapped_column(PGUUID(as_uuid=True), nullable=False)
    ordinal: Mapped[int] = mapped_column(Integer, nullable=False)
    text: Mapped[str] = mapped_column(Text, nullable=False)
    token_count: Mapped[int] = mapped_column(Integer, default=0, nullable=False)
    chunk_metadata: Mapped[dict[str, Any]] = mapped_column(JSONB, default=dict, nullable=False)
    # Dimension is fixed by the migration (RAG_EMBEDDING__DIM at migration time).
    embedding: Mapped[Any] = mapped_column(Vector(), nullable=True)

    document: Mapped[Document] = relationship(back_populates="chunks")


class SyncRun(UUIDPKMixin, Base):
    __tablename__ = "sync_runs"

    collection_id: Mapped[uuid.UUID] = mapped_column(
        ForeignKey("collections.id", ondelete="CASCADE"), nullable=False, index=True
    )
    trigger: Mapped[str] = mapped_column(String(10), default="cron", nullable=False)
    status: Mapped[str] = mapped_column(String(10), default="running", nullable=False)
    started_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False)
    finished_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    # {"<source name>": {"seen": n, "added": n, "updated": n, "deleted": n, "errors": [...]}}
    stats: Mapped[dict[str, Any]] = mapped_column(JSONB, default=dict, nullable=False)
    error: Mapped[str] = mapped_column(Text, default="", nullable=False)
