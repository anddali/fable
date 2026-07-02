"""Plugin contracts.

Every extension point is a small ABC discovered through Python entry points:

==============  ============================  ====================================
Kind            Entry-point group             Contract
==============  ============================  ====================================
connector       ``rag.connectors``            yields RawDocuments from a source
parser          ``rag.parsers``               bytes/mime -> ParsedDocument
transformer     ``rag.transformers``          ParsedDocument -> ParsedDocument
chunker         ``rag.chunkers``              ParsedDocument -> non-overlapping chunks
embedder        ``rag.embedders``             list[str] -> list[vector]
vision          ``rag.vision``                list[image] -> list[description]
==============  ============================  ====================================

A third-party package extends the platform by implementing one of these ABCs
and declaring it in its own ``pyproject.toml``::

    [project.entry-points."rag.connectors"]
    jira = "my_pkg.connector:JiraConnector"

Each plugin declares a pydantic ``config_model``; the platform validates the
operator-supplied JSON config against it before instantiation, so bad configs
fail at onboarding time, not at sync time.
"""

from __future__ import annotations

from abc import ABC, abstractmethod
from collections.abc import AsyncIterator
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, ClassVar

from pydantic import BaseModel

from rag_core.schemas import Block, ChunkDraft, ParsedDocument, RawDocument

if TYPE_CHECKING:
    from sqlalchemy.ext.asyncio import AsyncSession

    from rag_core.blobstore import BlobStore
    from rag_core.config import Settings


class EmptyConfig(BaseModel):
    """Default config for plugins that take no options (rejects unknown keys)."""

    model_config = {"extra": "forbid"}


class BasePlugin(ABC):
    """Common shape for every plugin kind."""

    #: Stable identifier; must match the entry-point name.
    name: ClassVar[str]
    #: Validates operator-supplied configuration. Override with a real model.
    config_model: ClassVar[type[BaseModel]] = EmptyConfig

    def __init__(self, config: BaseModel) -> None:
        self.config = config

    @classmethod
    def from_config(cls, raw_config: dict[str, Any]):
        return cls(cls.config_model.model_validate(raw_config))


@dataclass
class SyncContext:
    """Everything a connector may need while iterating a source."""

    settings: Settings
    session: AsyncSession
    blob_store: BlobStore
    source_id: Any  # UUID of the source row
    collection_id: Any
    #: Connector-owned cursor state, persisted on the source row between runs
    #: (e.g. Slack "latest ts", Graph delta links). Mutate freely.
    state: dict[str, Any] = field(default_factory=dict)


class ConnectorPlugin(BasePlugin):
    """Pulls documents from an external system.

    Implementations must yield *every currently existing* document each run
    (unless they maintain true incremental state via ``ctx.state``); documents
    that stop appearing are tombstoned by the sync engine. Secrets must never
    live in the stored config — reference environment variables instead
    (convention: a ``*_env`` config field holding the variable name).
    """

    @abstractmethod
    def iter_documents(self, ctx: SyncContext) -> AsyncIterator[RawDocument]: ...

    #: Set to True when iter_documents only yields changed items and deletion
    #: detection is handled through ctx.state rather than full enumeration.
    incremental: ClassVar[bool] = False

    #: True when the connector can derive source config from a pasted URL via
    #: ``parse_target_url`` (lets the UI offer a link field instead of raw JSON).
    supports_url_targets: ClassVar[bool] = False

    @classmethod
    def ready(cls, settings: Settings) -> bool:
        """Whether the connector is usable given platform config.

        Connectors that depend on operator-level configuration (a service-account
        connection, an API base URL) override this to report readiness, so the UI
        can gate the "add source" form instead of letting a doomed source be
        created. Default: always ready (self-contained connectors).
        """
        return True

    @classmethod
    def parse_target_url(cls, url: str, settings: Settings) -> dict[str, Any] | None:
        """Best-effort: turn a pasted URL into a source-config fragment.

        Returns a dict merged into the source config (e.g. a resolved target), or
        None if this connector can't interpret the URL. Pure/synchronous — no
        network; endpoints that need to follow redirects do so before calling.
        """
        return None


class ParserPlugin(BasePlugin):
    """Turns raw bytes/text into a structured ParsedDocument."""

    #: Mime types this parser handles when auto-selected.
    media_types: ClassVar[tuple[str, ...]] = ()
    #: When several parsers claim the same mime type, the highest wins
    #: auto-selection (explicit pipeline config bypasses this entirely).
    priority: ClassVar[int] = 0

    @abstractmethod
    def parse(self, raw: RawDocument) -> ParsedDocument: ...

    @staticmethod
    def decode(raw: RawDocument) -> str:
        if raw.text is not None:
            return raw.text
        return (raw.content or b"").decode("utf-8", errors="replace")


class TransformerPlugin(BasePlugin):
    """Cleans up / enriches a parsed document before chunking.

    Transformers run as an ordered chain configured per collection. Return a
    new or mutated ParsedDocument; return an empty ``blocks`` list to drop the
    document from the index entirely.
    """

    @abstractmethod
    def transform(self, doc: ParsedDocument) -> ParsedDocument: ...


class ChunkerPlugin(BasePlugin):
    """Splits a parsed document into chunks.

    Platform invariant: chunks MUST NOT overlap — every character of source
    text belongs to at most one chunk. ``Block`` boundaries are the natural
    split points.
    """

    @abstractmethod
    def chunk(self, doc: ParsedDocument) -> list[ChunkDraft]: ...


class EmbedderPlugin(BasePlugin):
    """Maps texts to fixed-dimension vectors."""

    @property
    @abstractmethod
    def dim(self) -> int: ...

    @abstractmethod
    async def embed(self, texts: list[str]) -> list[list[float]]: ...


class VisionPlugin(BasePlugin):
    """Describes images as text for indexing (figures, diagrams, charts).

    Receives ``(bytes, mime_type)`` pairs and returns one description per
    image, in order. An empty string means "nothing useful to say"; the
    pipeline drops that block instead of indexing noise.
    """

    @abstractmethod
    async def describe(self, images: list[tuple[bytes, str]]) -> list[str]: ...


__all__ = [
    "BasePlugin",
    "Block",
    "ChunkerPlugin",
    "ConnectorPlugin",
    "EmbedderPlugin",
    "ParserPlugin",
    "SyncContext",
    "TransformerPlugin",
    "VisionPlugin",
]
