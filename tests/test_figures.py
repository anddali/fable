"""Figure persistence and retrieval: chunker roll-up, indexer storage, key auth."""

import base64
import uuid
from types import SimpleNamespace

from rag_api.routers.search import parse_figure_key
from rag_core.builtin.chunkers.default import DefaultChunker
from rag_core.pipeline.indexer import _describe_images, figure_blob_key
from rag_core.schemas import Block, ParsedDocument, PipelineConfig, PluginRef

# ---------------------------------------------------------------------------
# Chunker: figure refs from packed blocks land in chunk_metadata["figures"]
# ---------------------------------------------------------------------------


def test_chunker_collects_figures_into_chunk_metadata():
    fig = {"key": "c/d/figures/abc-0.png", "page": 2}
    doc = ParsedDocument(
        blocks=[
            Block(type="heading", text="Flow", level=1),
            Block(text="intro"),
            Block(text="[Figure: a diagram]", metadata={"figure": fig}),
        ]
    )
    chunks = DefaultChunker.from_config({}).chunk(doc)
    assert len(chunks) == 1
    assert chunks[0].metadata["figures"] == [fig]


def test_chunker_no_figures_key_when_absent():
    doc = ParsedDocument(blocks=[Block(text="plain text")])
    chunks = DefaultChunker.from_config({}).chunk(doc)
    assert "figures" not in chunks[0].metadata


def test_chunker_figure_counted_once_for_split_blocks():
    fig = {"key": "c/d/figures/abc-0.png"}
    doc = ParsedDocument(
        blocks=[Block(text="word " * 4000, metadata={"figure": fig})]  # forces splitting
    )
    chunks = DefaultChunker.from_config({}).chunk(doc)
    assert len(chunks) > 1
    total_refs = sum(len(c.metadata.get("figures", [])) for c in chunks)
    assert total_refs == 1


# ---------------------------------------------------------------------------
# Indexer: described figures persisted to the blob store with refs
# ---------------------------------------------------------------------------


class MemoryBlobStore:
    def __init__(self):
        self.blobs = {}

    def put(self, key, data):
        self.blobs[key] = data

    def get(self, key):
        return self.blobs[key]

    def delete(self, key):
        self.blobs.pop(key, None)

    def exists(self, key):
        return key in self.blobs


class FakeVision:
    def __init__(self, descriptions):
        self.descriptions = descriptions

    async def describe(self, images):
        return self.descriptions


def _doc():
    return SimpleNamespace(
        id=uuid.UUID("00000000-0000-0000-0000-00000000000d"),
        collection_id=uuid.UUID("00000000-0000-0000-0000-00000000000c"),
        content_hash="49109583882287d7e93094ca03f52648",
    )


def _image_block(data=b"png", page=3):
    return Block(
        type="image",
        text="",
        metadata={
            "image_b64": base64.b64encode(data).decode(),
            "image_mime": "image/png",
            "page": page,
        },
    )


async def test_described_figure_persisted_and_referenced(monkeypatch):
    monkeypatch.setattr(
        "rag_core.pipeline.indexer.plugins.create",
        lambda kind, name, config: FakeVision(["a diagram"]),
    )
    store = MemoryBlobStore()
    document = _doc()
    parsed = ParsedDocument(blocks=[_image_block(b"png-bytes")])
    result = await _describe_images(
        parsed, PipelineConfig(vision_llm=PluginRef(name="x")), store, document
    )

    expected_key = figure_blob_key(
        document.collection_id, document.id, document.content_hash, 0
    )
    assert store.blobs[expected_key] == b"png-bytes"
    block = result.blocks[0]
    assert block.metadata["figure"] == {"key": expected_key, "page": 3}
    assert "image_b64" not in block.metadata


async def test_undescribed_figure_not_persisted(monkeypatch):
    monkeypatch.setattr(
        "rag_core.pipeline.indexer.plugins.create",
        lambda kind, name, config: FakeVision([""]),
    )
    store = MemoryBlobStore()
    parsed = ParsedDocument(blocks=[_image_block()])
    result = await _describe_images(
        parsed, PipelineConfig(vision_llm=PluginRef(name="x")), store, _doc()
    )
    assert store.blobs == {}
    assert result.blocks == []


async def test_no_vision_plugin_persists_nothing():
    store = MemoryBlobStore()
    parsed = ParsedDocument(blocks=[_image_block()])
    result = await _describe_images(parsed, PipelineConfig(), store, _doc())
    assert store.blobs == {}
    assert result.blocks == []


# ---------------------------------------------------------------------------
# Figures endpoint key validation: only figure keys, never payload blobs
# ---------------------------------------------------------------------------


def test_parse_figure_key_accepts_indexer_keys():
    document = _doc()
    key = figure_blob_key(document.collection_id, document.id, document.content_hash, 4)
    assert parse_figure_key(key) == document.collection_id


def test_parse_figure_key_rejects_payload_and_malformed_keys():
    cid, did = uuid.uuid4(), uuid.uuid4()
    bad = [
        f"{cid}/{did}/49109583882287d7e93094ca03f52648",  # raw document payload
        f"uploads/{cid}/{did}/49109583",  # upload staging blob
        f"{cid}/{did}/figures/../../../etc/passwd",
        f"{cid}/{did}/figures/abc-0.jpg",  # wrong extension
        "not-a-uuid/x/figures/ab-0.png",
        "",
    ]
    assert all(parse_figure_key(k) is None for k in bad)
