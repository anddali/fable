"""initial schema

Revision ID: 0001
Revises:
Create Date: 2026-06-09
"""

from __future__ import annotations

import sqlalchemy as sa
from alembic import op
from pgvector.sqlalchemy import Vector
from sqlalchemy.dialects.postgresql import JSONB, UUID

from rag_core.config import get_settings

revision = "0001"
down_revision = None
branch_labels = None
depends_on = None

# Vector dimension is fixed at migration time from RAG_EMBEDDING__DIM.
# Changing it later means a new migration + full reindex.
EMBEDDING_DIM = get_settings().embedding.dim


def upgrade() -> None:
    op.execute("CREATE EXTENSION IF NOT EXISTS vector")

    op.create_table(
        "collections",
        sa.Column("id", UUID(as_uuid=True), primary_key=True),
        sa.Column("name", sa.String(200), nullable=False, unique=True),
        sa.Column("description", sa.Text(), nullable=False, server_default=""),
        sa.Column("pipeline", JSONB(), nullable=False, server_default="{}"),
        sa.Column("sync_cron", sa.String(100), nullable=False, server_default="0 2 * * *"),
        sa.Column("sync_enabled", sa.Boolean(), nullable=False, server_default="true"),
        sa.Column("next_sync_at", sa.DateTime(timezone=True)),
        sa.Column("created_by", sa.String(100), nullable=False, server_default=""),
        sa.Column(
            "created_at",
            sa.DateTime(timezone=True),
            nullable=False,
            server_default=sa.text("now()"),
        ),
        sa.Column(
            "updated_at",
            sa.DateTime(timezone=True),
            nullable=False,
            server_default=sa.text("now()"),
        ),
    )

    op.create_table(
        "collection_principals",
        sa.Column("id", UUID(as_uuid=True), primary_key=True),
        sa.Column(
            "collection_id",
            UUID(as_uuid=True),
            sa.ForeignKey("collections.id", ondelete="CASCADE"),
            nullable=False,
            index=True,
        ),
        sa.Column("principal_type", sa.String(10), nullable=False),
        sa.Column("principal_id", sa.String(100), nullable=False),
        sa.Column("role", sa.String(10), nullable=False),
        sa.Column(
            "created_at",
            sa.DateTime(timezone=True),
            nullable=False,
            server_default=sa.text("now()"),
        ),
        sa.Column(
            "updated_at",
            sa.DateTime(timezone=True),
            nullable=False,
            server_default=sa.text("now()"),
        ),
        sa.UniqueConstraint("collection_id", "principal_type", "principal_id"),
        sa.CheckConstraint("principal_type IN ('user', 'group')", name="principal_type_valid"),
        sa.CheckConstraint("role IN ('owner', 'editor', 'viewer')", name="role_valid"),
    )

    op.create_table(
        "sources",
        sa.Column("id", UUID(as_uuid=True), primary_key=True),
        sa.Column(
            "collection_id",
            UUID(as_uuid=True),
            sa.ForeignKey("collections.id", ondelete="CASCADE"),
            nullable=False,
            index=True,
        ),
        sa.Column("name", sa.String(200), nullable=False),
        sa.Column("connector", sa.String(100), nullable=False),
        sa.Column("config", JSONB(), nullable=False, server_default="{}"),
        sa.Column("enabled", sa.Boolean(), nullable=False, server_default="true"),
        sa.Column("state", JSONB(), nullable=False, server_default="{}"),
        sa.Column("last_synced_at", sa.DateTime(timezone=True)),
        sa.Column(
            "created_at",
            sa.DateTime(timezone=True),
            nullable=False,
            server_default=sa.text("now()"),
        ),
        sa.Column(
            "updated_at",
            sa.DateTime(timezone=True),
            nullable=False,
            server_default=sa.text("now()"),
        ),
        sa.UniqueConstraint("collection_id", "name"),
    )

    op.create_table(
        "upload_items",
        sa.Column("id", UUID(as_uuid=True), primary_key=True),
        sa.Column(
            "source_id",
            UUID(as_uuid=True),
            sa.ForeignKey("sources.id", ondelete="CASCADE"),
            nullable=False,
            index=True,
        ),
        sa.Column("filename", sa.String(500), nullable=False),
        sa.Column("mime_type", sa.String(200), nullable=False),
        sa.Column("size_bytes", sa.BigInteger(), nullable=False),
        sa.Column("blob_key", sa.String(800), nullable=False),
        sa.Column("content_hash", sa.String(64), nullable=False),
        sa.Column("uploaded_by", sa.String(100), nullable=False, server_default=""),
        sa.Column("deleted", sa.Boolean(), nullable=False, server_default="false"),
        sa.Column(
            "created_at",
            sa.DateTime(timezone=True),
            nullable=False,
            server_default=sa.text("now()"),
        ),
        sa.Column(
            "updated_at",
            sa.DateTime(timezone=True),
            nullable=False,
            server_default=sa.text("now()"),
        ),
    )

    op.create_table(
        "documents",
        sa.Column("id", UUID(as_uuid=True), primary_key=True),
        sa.Column(
            "source_id",
            UUID(as_uuid=True),
            sa.ForeignKey("sources.id", ondelete="CASCADE"),
            nullable=False,
            index=True,
        ),
        sa.Column(
            "collection_id",
            UUID(as_uuid=True),
            sa.ForeignKey("collections.id", ondelete="CASCADE"),
            nullable=False,
            index=True,
        ),
        sa.Column("external_id", sa.String(800), nullable=False),
        sa.Column("title", sa.String(1000), nullable=False, server_default=""),
        sa.Column("uri", sa.String(2000), nullable=False, server_default=""),
        sa.Column("mime_type", sa.String(200), nullable=False, server_default="text/plain"),
        sa.Column("content_hash", sa.String(64), nullable=False, server_default=""),
        sa.Column("size_bytes", sa.BigInteger(), nullable=False, server_default="0"),
        sa.Column("blob_key", sa.String(800), nullable=False, server_default=""),
        sa.Column("doc_metadata", JSONB(), nullable=False, server_default="{}"),
        sa.Column("status", sa.String(10), nullable=False, server_default="pending"),
        sa.Column("error", sa.Text(), nullable=False, server_default=""),
        sa.Column("source_updated_at", sa.DateTime(timezone=True)),
        sa.Column("indexed_at", sa.DateTime(timezone=True)),
        sa.Column(
            "created_at",
            sa.DateTime(timezone=True),
            nullable=False,
            server_default=sa.text("now()"),
        ),
        sa.Column(
            "updated_at",
            sa.DateTime(timezone=True),
            nullable=False,
            server_default=sa.text("now()"),
        ),
        sa.UniqueConstraint("source_id", "external_id"),
        sa.CheckConstraint(
            "status IN ('pending', 'indexed', 'failed', 'deleted')", name="status_valid"
        ),
    )

    op.create_table(
        "chunks",
        sa.Column("id", UUID(as_uuid=True), primary_key=True),
        sa.Column(
            "document_id",
            UUID(as_uuid=True),
            sa.ForeignKey("documents.id", ondelete="CASCADE"),
            nullable=False,
            index=True,
        ),
        sa.Column("collection_id", UUID(as_uuid=True), nullable=False),
        sa.Column("ordinal", sa.Integer(), nullable=False),
        sa.Column("text", sa.Text(), nullable=False),
        sa.Column("token_count", sa.Integer(), nullable=False, server_default="0"),
        sa.Column("chunk_metadata", JSONB(), nullable=False, server_default="{}"),
        sa.Column("embedding", Vector(EMBEDDING_DIM), nullable=True),
        sa.UniqueConstraint("document_id", "ordinal"),
    )
    op.create_index("ix_chunks_collection_id", "chunks", ["collection_id"])
    # Lexical leg of hybrid search: generated tsvector + GIN.
    op.execute(
        "ALTER TABLE chunks ADD COLUMN tsv tsvector "
        "GENERATED ALWAYS AS (to_tsvector('english', text)) STORED"
    )
    op.execute("CREATE INDEX ix_chunks_tsv ON chunks USING gin (tsv)")
    # ANN index for vector search (cosine).
    op.execute(
        "CREATE INDEX ix_chunks_embedding ON chunks "
        "USING hnsw (embedding vector_cosine_ops) WITH (m = 16, ef_construction = 64)"
    )

    op.create_table(
        "sync_runs",
        sa.Column("id", UUID(as_uuid=True), primary_key=True),
        sa.Column(
            "collection_id",
            UUID(as_uuid=True),
            sa.ForeignKey("collections.id", ondelete="CASCADE"),
            nullable=False,
            index=True,
        ),
        sa.Column("trigger", sa.String(10), nullable=False, server_default="cron"),
        sa.Column("status", sa.String(10), nullable=False, server_default="running"),
        sa.Column("started_at", sa.DateTime(timezone=True), nullable=False),
        sa.Column("finished_at", sa.DateTime(timezone=True)),
        sa.Column("stats", JSONB(), nullable=False, server_default="{}"),
        sa.Column("error", sa.Text(), nullable=False, server_default=""),
        sa.CheckConstraint("status IN ('running', 'succeeded', 'failed')", name="status_valid"),
    )


def downgrade() -> None:
    op.drop_table("sync_runs")
    op.drop_table("chunks")
    op.drop_table("documents")
    op.drop_table("upload_items")
    op.drop_table("sources")
    op.drop_table("collection_principals")
    op.drop_table("collections")
