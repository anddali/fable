"""Central runtime configuration.

Everything is driven by environment variables with the ``RAG_`` prefix and
``__`` as the nesting delimiter, e.g. ``RAG_AUTH__TENANT_ID``.
"""

from __future__ import annotations

from functools import lru_cache
from typing import Literal

from pydantic import BaseModel, Field
from pydantic_settings import BaseSettings, SettingsConfigDict


class AuthSettings(BaseModel):
    # "entra" validates Microsoft Entra ID bearer tokens; "disabled" injects a
    # local dev principal and must never be used outside development.
    mode: Literal["entra", "disabled"] = "entra"
    tenant_id: str = ""
    # Audience expected in access tokens (the API app registration's client id
    # or Application ID URI).
    audience: str = ""
    # Entra app roles -> platform global roles. Assign these app roles to
    # users/groups in the Entra enterprise application.
    role_map: dict[str, str] = Field(
        default_factory=lambda: {
            "Rag.Admin": "admin",
            "Rag.User": "user",
        }
    )
    # Role granted to authenticated users whose token carries no mapped app
    # role. Set to "" to require explicit app-role assignment instead.
    default_role: str = "user"
    # Client id of the SPA app registration used by the management UI
    # (authorization code + PKCE via MSAL in the browser).
    spa_client_id: str = ""

    @property
    def issuer(self) -> str:
        return f"https://login.microsoftonline.com/{self.tenant_id}/v2.0"

    @property
    def jwks_uri(self) -> str:
        return f"https://login.microsoftonline.com/{self.tenant_id}/discovery/v2.0/keys"


class EmbeddingSettings(BaseModel):
    """Platform-wide default embedder.

    A collection may override transformer/chunker plugins freely, but all
    collections share one embedding space (single vector column dimension).
    Changing provider/dim requires a migration + full reindex.
    """

    plugin: str = "openai"
    dim: int = 1536
    config: dict = Field(
        default_factory=lambda: {"model": "text-embedding-3-small", "api_key_env": "OPENAI_API_KEY"}
    )


class Settings(BaseSettings):
    model_config = SettingsConfigDict(
        env_prefix="RAG_", env_nested_delimiter="__", env_file=".env", extra="ignore"
    )

    environment: Literal["dev", "staging", "prod"] = "dev"
    log_level: str = "INFO"

    # SQLAlchemy URL (async, psycopg3): postgresql+psycopg://user:pass@host/db
    database_url: str = "postgresql+psycopg://rag:rag@localhost:5432/rag"

    # Filesystem blob store root (raw source bytes + manual uploads).
    # Mount a persistent volume here; swap for an S3 implementation via
    # rag_core.blobstore.BlobStore when needed.
    blob_dir: str = "var/blobs"

    # Working directory for connectors that need local scratch space (git clones).
    cache_dir: str = "var/cache"

    auth: AuthSettings = Field(default_factory=AuthSettings)
    embedding: EmbeddingSettings = Field(default_factory=EmbeddingSettings)

    # Hard ceilings applied regardless of per-collection pipeline config.
    max_upload_bytes: int = 50 * 1024 * 1024
    search_max_top_k: int = 50

    @property
    def procrastinate_dsn(self) -> str:
        """Plain libpq DSN for procrastinate (no SQLAlchemy dialect prefix)."""
        return self.database_url.replace("postgresql+psycopg://", "postgresql://", 1)


@lru_cache
def get_settings() -> Settings:
    return Settings()
