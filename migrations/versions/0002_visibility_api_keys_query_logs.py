"""collection visibility, api keys, query logs

Revision ID: 0002
Revises: 0001
Create Date: 2026-06-10
"""

from __future__ import annotations

import sqlalchemy as sa
from alembic import op
from sqlalchemy.dialects.postgresql import JSONB, UUID

revision = "0002"
down_revision = "0001"
branch_labels = None
depends_on = None


def upgrade() -> None:
    op.add_column(
        "collections",
        sa.Column("visibility", sa.String(10), nullable=False, server_default="private"),
    )
    op.create_check_constraint(
        "visibility_valid", "collections", "visibility IN ('private', 'public')"
    )

    op.create_table(
        "api_keys",
        sa.Column("id", UUID(as_uuid=True), primary_key=True),
        sa.Column("name", sa.String(200), nullable=False),
        sa.Column("prefix", sa.String(16), nullable=False),
        sa.Column("key_hash", sa.String(64), nullable=False, unique=True),
        sa.Column("owner_oid", sa.String(100), nullable=False, index=True),
        sa.Column("owner_name", sa.String(200), nullable=False, server_default=""),
        sa.Column("scopes", JSONB(), nullable=False, server_default="[]"),
        sa.Column("revoked", sa.Boolean(), nullable=False, server_default="false"),
        sa.Column("expires_at", sa.DateTime(timezone=True)),
        sa.Column("last_used_at", sa.DateTime(timezone=True)),
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
        "query_logs",
        sa.Column("id", UUID(as_uuid=True), primary_key=True),
        sa.Column("principal_oid", sa.String(100), nullable=False, index=True),
        sa.Column("principal_name", sa.String(200), nullable=False, server_default=""),
        sa.Column("auth_method", sa.String(10), nullable=False, server_default="sso"),
        sa.Column("api_key_id", UUID(as_uuid=True)),
        sa.Column("query", sa.Text(), nullable=False),
        sa.Column("mode", sa.String(10), nullable=False, server_default="hybrid"),
        sa.Column("top_k", sa.Integer(), nullable=False, server_default="0"),
        sa.Column("collection_ids", JSONB(), nullable=False, server_default="[]"),
        sa.Column("hit_count", sa.Integer(), nullable=False, server_default="0"),
        sa.Column("latency_ms", sa.Integer(), nullable=False, server_default="0"),
        sa.Column(
            "created_at",
            sa.DateTime(timezone=True),
            nullable=False,
            server_default=sa.text("now()"),
            index=True,
        ),
    )


def downgrade() -> None:
    op.drop_table("query_logs")
    op.drop_table("api_keys")
    op.drop_constraint("ck_collections_visibility_valid", "collections")
    op.drop_column("collections", "visibility")
