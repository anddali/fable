"""Document indexing: blob -> parse -> transform chain -> chunk -> embed -> persist.

Idempotent and re-runnable: chunks are replaced atomically inside the caller's
transaction, so a failed run leaves the previous index state intact.
"""

from __future__ import annotations

import asyncio
import base64
import uuid
from datetime import UTC, datetime

import structlog
from sqlalchemy import delete, select
from sqlalchemy.ext.asyncio import AsyncSession

from rag_core import plugins
from rag_core.blobstore import BlobStore
from rag_core.config import Settings
from rag_core.db.models import Chunk, Collection, Document, Source, UploadItem
from rag_core.schemas import Block, ParsedDocument, PipelineConfig, RawDocument

log = structlog.get_logger(__name__)


def get_embedder(settings: Settings):
    cfg = dict(settings.embedding.config)
    cfg.setdefault("dim", settings.embedding.dim)
    return plugins.create("embedder", settings.embedding.plugin, cfg)


async def index_document(
    session: AsyncSession,
    settings: Settings,
    blob_store: BlobStore,
    document_id: uuid.UUID | str,
) -> int:
    """(Re)index one document. Returns the number of chunks written."""
    document = await session.get(Document, document_id)
    if document is None:
        # Index jobs are enqueued only after the transaction creating the row
        # commits, so a missing row means the document was deleted since (e.g.
        # a collection cascade). Raise so the queue's retry policy covers any
        # residual visibility race instead of recording a silent success.
        log.warning("index.document_missing", document_id=str(document_id))
        raise RuntimeError(f"Document {document_id} not found")
    if document.status == "deleted":
        return 0

    collection = await session.get(Collection, document.collection_id)
    pipeline = PipelineConfig.model_validate(collection.pipeline or {})

    try:
        raw = RawDocument(
            external_id=document.external_id,
            title=document.title,
            uri=document.uri,
            mime_type=document.mime_type,
            content=blob_store.get(document.blob_key),
            metadata=dict(document.doc_metadata),
            content_hash=document.content_hash,
        )

        if pipeline.parser:
            parser = plugins.create("parser", pipeline.parser.name, pipeline.parser.config)
        else:
            parser = plugins.select_parser(document.mime_type)
        # Parsing is CPU-bound (docling runs layout/table models for seconds
        # per page); keep the worker event loop responsive.
        parsed = await asyncio.to_thread(parser.parse, raw)

        parsed = await _describe_images(parsed, pipeline, blob_store, document)

        for ref in pipeline.transformers:
            transformer = plugins.create("transformer", ref.name, ref.config)
            parsed = transformer.transform(parsed)

        chunker = plugins.create("chunker", pipeline.chunker.name, pipeline.chunker.config)
        drafts = chunker.chunk(parsed)

        embedder = get_embedder(settings)
        vectors = await embedder.embed([d.text for d in drafts]) if drafts else []
        if len(vectors) != len(drafts):
            raise RuntimeError(f"Embedder returned {len(vectors)} vectors for {len(drafts)} chunks")

        old_figure_keys = await _figure_keys(session, document.id)
        await session.execute(delete(Chunk).where(Chunk.document_id == document.id))
        session.add_all(
            Chunk(
                document_id=document.id,
                collection_id=document.collection_id,
                ordinal=draft.ordinal,
                text=draft.text,
                token_count=draft.token_count,
                chunk_metadata=draft.metadata,
                embedding=vector,
            )
            for draft, vector in zip(drafts, vectors, strict=True)
        )
        # Drop figure crops superseded by this run (same pattern as payload
        # blob cleanup in sync). Unchanged content reuses identical keys, so
        # the difference set is empty on a no-op reindex.
        new_figure_keys = {
            f["key"] for d in drafts for f in d.metadata.get("figures", []) if f.get("key")
        }
        for stale in old_figure_keys - new_figure_keys:
            blob_store.delete(stale)

        document.status = "indexed"
        document.error = ""
        document.indexed_at = datetime.now(UTC)
        log.info(
            "index.done",
            document_id=str(document.id),
            chunks=len(drafts),
            parser=type(parser).name,
        )
        return len(drafts)
    except Exception as exc:
        # Mark failure on the row but re-raise so the queue records/retries it.
        document.status = "failed"
        document.error = str(exc)[:4000]
        log.exception("index.failed", document_id=str(document.id))
        raise


def figure_blob_key(collection_id, document_id, content_hash: str, n: int) -> str:
    """Deterministic key: same content -> same keys, so reindex overwrites
    in place and the stale-key difference set stays empty."""
    return f"{collection_id}/{document_id}/figures/{content_hash[:16]}-{n}.png"


async def _describe_images(
    parsed: ParsedDocument,
    pipeline: PipelineConfig,
    blob_store: BlobStore,
    document: Document,
) -> ParsedDocument:
    """Turn ``image`` blocks into text via the collection's vision plugin.

    Described figures keep their cropped image: the PNG is persisted to the
    blob store and referenced from the block metadata (``figure``), which the
    chunker rolls up into ``chunk_metadata["figures"]`` — that's what lets
    search hits link back to the actual image. Without a configured
    ``vision_llm`` (or for images the plugin has nothing to say about) image
    blocks are dropped. Raw image bytes never reach the database.
    """
    image_blocks = [b for b in parsed.blocks if b.type == "image"]
    descriptions: dict[int, str] = {}
    if image_blocks and pipeline.vision_llm:
        vision = plugins.create("vision", pipeline.vision_llm.name, pipeline.vision_llm.config)
        images = [
            (
                base64.b64decode(b.metadata.get("image_b64", "")),
                b.metadata.get("image_mime", "image/png"),
            )
            for b in image_blocks
        ]
        results = await vision.describe(images)
        descriptions = {id(b): desc for b, desc in zip(image_blocks, results, strict=True)}

    blocks: list[Block] = []
    figure_n = 0
    for block in parsed.blocks:
        if block.type != "image":
            blocks.append(block)
            continue
        description = descriptions.get(id(block), "").strip()
        if not description:
            continue
        key = figure_blob_key(document.collection_id, document.id, document.content_hash, figure_n)
        figure_n += 1
        blob_store.put(key, base64.b64decode(block.metadata.get("image_b64", "")))
        metadata = {k: v for k, v in block.metadata.items() if k not in ("image_b64", "image_mime")}
        metadata["figure"] = {
            "key": key,
            **({"page": metadata["page"]} if "page" in metadata else {}),
        }
        blocks.append(Block(type="paragraph", text=f"[Figure: {description}]", metadata=metadata))
    parsed.blocks = blocks
    return parsed


async def _figure_keys(session: AsyncSession, document_id: uuid.UUID | str) -> set[str]:
    """Figure blob keys referenced by a document's current chunks."""
    result = await session.execute(
        select(Chunk.chunk_metadata).where(Chunk.document_id == document_id)
    )
    return {f["key"] for (meta,) in result for f in (meta or {}).get("figures", []) if f.get("key")}


async def remove_document_chunks(
    session: AsyncSession, document_id: uuid.UUID, blob_store: BlobStore | None = None
) -> None:
    if blob_store is not None:
        for key in await _figure_keys(session, document_id):
            blob_store.delete(key)
    await session.execute(delete(Chunk).where(Chunk.document_id == document_id))


async def collection_blob_keys(session: AsyncSession, collection_id: uuid.UUID) -> set[str]:
    """Every blob key owned by a collection: document payloads + figure images.

    Used to purge the blob store when a collection is deleted. The DB-level
    ``ON DELETE CASCADE`` removes rows only, so blobs must be collected before
    the rows vanish and deleted afterwards. Store-agnostic: keys come from the
    rows themselves, not from the on-disk layout."""
    keys: set[str] = set()
    payloads = await session.execute(
        select(Document.blob_key).where(Document.collection_id == collection_id)
    )
    keys.update(k for (k,) in payloads if k)
    figures = await session.execute(
        select(Chunk.chunk_metadata).where(Chunk.collection_id == collection_id)
    )
    keys.update(
        f["key"] for (meta,) in figures for f in (meta or {}).get("figures", []) if f.get("key")
    )
    # Staging blobs for manual uploads live under uploads/{source}/... — keyed by
    # source, not collection/document, so they aren't covered by the rows above.
    uploads = await session.execute(
        select(UploadItem.blob_key)
        .join(Source, Source.id == UploadItem.source_id)
        .where(Source.collection_id == collection_id)
    )
    keys.update(k for (k,) in uploads if k)
    return keys


async def documents_pending(session: AsyncSession, collection_id: uuid.UUID) -> list[uuid.UUID]:
    result = await session.execute(
        select(Document.id).where(
            Document.collection_id == collection_id, Document.status == "pending"
        )
    )
    return list(result.scalars())
