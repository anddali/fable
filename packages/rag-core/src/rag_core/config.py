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
    # Delegated scope the SPA requests, e.g. "api://<api-client-id>/access_as_user".
    # Empty = derived as "<audience>/.default".
    api_scope: str = ""

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


class ChatSettings(BaseModel):
    """Answer-synthesis LLM for the console Chat page (``/chat``).

    Any OpenAI-compatible ``/chat/completions`` server works — OpenAI, a local
    Ollama (``http://host.docker.internal:11434/v1``), vLLM, Azure's v1 surface.
    Disabled by default: set ``enabled=true`` plus a ``base_url``/``model`` (and
    ``api_key_env`` when the server authenticates) to turn the feature on.
    """

    enabled: bool = False
    base_url: str = "https://api.openai.com/v1"
    model: str = "gpt-4o-mini"
    # Name of the env var holding the API key; may be unset for local servers.
    api_key_env: str = "OPENAI_API_KEY"
    temperature: float = 0.2
    max_tokens: int = 800
    timeout_seconds: float = 120.0
    # Retrieval breadth and fusion mode for the grounding context of each turn.
    top_k: int = 6
    mode: Literal["vector", "hybrid"] = "hybrid"
    # Prior exchanges (user+assistant) kept as conversation memory per request.
    history_turns: int = 3


class ConfluenceConnection(BaseModel):
    """One Confluence service account (the operator-owned side of a source).

    Secrets stay out of the DB and out of this object: ``api_token_env`` names an
    environment variable holding the Atlassian API token, read at sync time.
    """

    base_url: str = ""  # e.g. https://yourorg.atlassian.net/wiki
    user_email: str = ""  # service-account email (Atlassian basic auth username)
    api_token_env: str = "CONFLUENCE_API_TOKEN"

    @property
    def configured(self) -> bool:
        return bool(self.base_url and self.user_email)


class ConfluenceSettings(BaseModel):
    """Platform-wide Confluence connection(s).

    The flat ``base_url``/``user_email``/``api_token_env`` fields define the
    ``"default"`` connection — the only one most deployments need. ``connections``
    is the door-open hook: a map of extra named connections (e.g. a scoped
    service account for a restricted space) resolvable without any DB table.
    Sources select one via their optional ``connection`` field.
    """

    base_url: str = ""
    user_email: str = ""
    api_token_env: str = "CONFLUENCE_API_TOKEN"
    connections: dict[str, ConfluenceConnection] = Field(default_factory=dict)

    def connection(self, name: str = "default") -> ConfluenceConnection:
        """Resolve a connection by name. The single place name->connection maps."""
        if name in self.connections:
            return self.connections[name]
        if name == "default":
            return ConfluenceConnection(
                base_url=self.base_url,
                user_email=self.user_email,
                api_token_env=self.api_token_env,
            )
        raise KeyError(name)


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
    chat: ChatSettings = Field(default_factory=ChatSettings)
    confluence: ConfluenceSettings = Field(default_factory=ConfluenceSettings)

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
