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
from rag_core.db.models import Chunk, Collection, Document
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
        log.warning("index.document_missing", document_id=str(document_id))
        return 0
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

        parsed = await _describe_images(parsed, pipeline)

        for ref in pipeline.transformers:
            transformer = plugins.create("transformer", ref.name, ref.config)
            parsed = transformer.transform(parsed)

        chunker = plugins.create("chunker", pipeline.chunker.name, pipeline.chunker.config)
        drafts = chunker.chunk(parsed)

        embedder = get_embedder(settings)
        vectors = await embedder.embed([d.text for d in drafts]) if drafts else []
        if len(vectors) != len(drafts):
            raise RuntimeError(f"Embedder returned {len(vectors)} vectors for {len(drafts)} chunks")

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


async def _describe_images(parsed: ParsedDocument, pipeline: PipelineConfig) -> ParsedDocument:
    """Turn ``image`` blocks into text via the collection's vision plugin.

    Without a configured ``vision_llm`` (or for images the plugin returns
    nothing useful about) image blocks are dropped. Image bytes live only in
    block metadata during indexing and never reach the database.
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
    for block in parsed.blocks:
        if block.type != "image":
            blocks.append(block)
            continue
        description = descriptions.get(id(block), "").strip()
        if not description:
            continue
        metadata = {k: v for k, v in block.metadata.items() if k not in ("image_b64", "image_mime")}
        blocks.append(Block(type="paragraph", text=f"[Figure: {description}]", metadata=metadata))
    parsed.blocks = blocks
    return parsed


async def remove_document_chunks(session: AsyncSession, document_id: uuid.UUID) -> None:
    await session.execute(delete(Chunk).where(Chunk.document_id == document_id))


async def documents_pending(session: AsyncSession, collection_id: uuid.UUID) -> list[uuid.UUID]:
    result = await session.execute(
        select(Document.id).where(
            Document.collection_id == collection_id, Document.status == "pending"
        )
    )
    return list(result.scalars())
