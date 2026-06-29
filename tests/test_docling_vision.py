"""Docling block mapping, vision plugins, and the indexer's figure-description step.

The mapping tests drive ``blocks_from_docling`` with duck-typed stubs so they
run without the docling models installed.
"""

import base64
import io
import json
import uuid
from dataclasses import dataclass, field
from typing import Any

import httpx
import pytest

from rag_core.builtin.parsers.docling import blocks_from_docling
from rag_core.builtin.vision.anthropic import AnthropicVision
from rag_core.builtin.vision.openai_compat import OpenAICompatVision
from rag_core.pipeline.indexer import _describe_images
from rag_core.schemas import Block, ParsedDocument, PipelineConfig, PluginRef

# ---------------------------------------------------------------------------
# Docling block mapping (stubs mimic DoclingDocument items)
# ---------------------------------------------------------------------------


@dataclass
class Prov:
    page_no: int = 1


@dataclass
class Item:
    label: str = "text"
    text: str = ""
    level: int = 1
    prov: list = field(default_factory=lambda: [Prov()])


@dataclass
class Cell:
    text: str = ""


@dataclass
class TableData:
    grid: list = field(default_factory=list)


@dataclass
class TableItem:
    label: str = "table"
    data: TableData = field(default_factory=TableData)
    prov: list = field(default_factory=lambda: [Prov()])


class FakeImage:
    def save(self, buf, format="PNG"):
        buf.write(b"png-bytes")


@dataclass
class PictureItem:
    label: str = "picture"
    image: Any = field(default_factory=FakeImage)
    prov: list = field(default_factory=lambda: [Prov(page_no=3)])

    def get_image(self, doc):
        return self.image


class StubDoc:
    def __init__(self, items):
        self._items = items

    def iterate_items(self):
        return [(item, 0) for item in self._items]


def test_mapping_headings_paragraphs_and_pages():
    doc = StubDoc(
        [
            Item(label="title", text="My Report"),
            Item(label="section_header", text="Intro", level=1),
            Item(label="text", text="Hello world.", prov=[Prov(page_no=2)]),
            Item(label="code", text="print('x')"),
            Item(label="list_item", text="point one"),
            Item(label="text", text="   "),  # whitespace-only: dropped
        ]
    )
    blocks = blocks_from_docling(doc)
    assert [(b.type, b.text) for b in blocks] == [
        ("heading", "My Report"),
        ("heading", "Intro"),
        ("paragraph", "Hello world."),
        ("code", "print('x')"),
        ("list_item", "point one"),
    ]
    assert blocks[0].level == 1
    assert blocks[1].level == 2  # sections sit below the title
    assert blocks[2].metadata["page"] == 2


def test_mapping_table_rows_join_header_and_values():
    grid = [
        [Cell("Name"), Cell("Role")],
        [Cell("Ada"), Cell("Engineer")],
        [Cell("Grace"), Cell("")],
    ]
    doc = StubDoc([TableItem(data=TableData(grid=grid))])
    blocks = blocks_from_docling(doc)
    assert [b.text for b in blocks] == ["Name: Ada | Role: Engineer", "Name: Grace"]
    assert all(b.type == "table_row" for b in blocks)


def test_mapping_picture_becomes_image_block():
    doc = StubDoc([PictureItem()])
    blocks = blocks_from_docling(doc)
    assert len(blocks) == 1
    block = blocks[0]
    assert block.type == "image"
    assert base64.b64decode(block.metadata["image_b64"]) == b"png-bytes"
    assert block.metadata["image_mime"] == "image/png"
    assert block.metadata["page"] == 3


def test_mapping_extract_images_false_drops_pictures():
    doc = StubDoc([PictureItem(), Item(label="text", text="kept")])
    blocks = blocks_from_docling(doc, extract_images=False)
    assert [b.text for b in blocks] == ["kept"]


# ---------------------------------------------------------------------------
# Vision plugins (httpx.MockTransport)
# ---------------------------------------------------------------------------


def _patch_async_client(monkeypatch, handler):
    transport = httpx.MockTransport(handler)
    real = httpx.AsyncClient

    def patched(**kwargs):
        kwargs["transport"] = transport
        return real(**kwargs)

    monkeypatch.setattr(httpx, "AsyncClient", patched)


async def test_openai_compat_vision_request_shape_and_optional_key(monkeypatch):
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["url"] = str(request.url)
        seen["auth"] = request.headers.get("authorization")
        seen["payload"] = json.loads(request.content)
        return httpx.Response(
            200, json={"choices": [{"message": {"content": " A bar chart. "}}]}
        )

    _patch_async_client(monkeypatch, handler)
    monkeypatch.delenv("OLLAMA_API_KEY", raising=False)

    vision = OpenAICompatVision.from_config(
        {"base_url": "http://localhost:11434/v1", "model": "gemma4:latest", "api_key_env": "OLLAMA_API_KEY"}
    )
    out = await vision.describe([(b"img", "image/png")])

    assert out == ["A bar chart."]
    assert seen["url"] == "http://localhost:11434/v1/chat/completions"
    assert seen["auth"] is None  # no key set -> no Authorization header
    content = seen["payload"]["messages"][0]["content"]
    assert content[1]["image_url"]["url"].startswith("data:image/png;base64,")


async def test_openai_compat_vision_retries_on_429(monkeypatch):
    calls = {"n": 0}

    def handler(request: httpx.Request) -> httpx.Response:
        calls["n"] += 1
        if calls["n"] == 1:
            return httpx.Response(429, json={})
        return httpx.Response(200, json={"choices": [{"message": {"content": "ok"}}]})

    _patch_async_client(monkeypatch, handler)
    monkeypatch.setattr("rag_core.builtin.vision.openai_compat.asyncio.sleep", _no_sleep)

    vision = OpenAICompatVision.from_config({"api_key_env": "NOPE_UNSET"})
    assert await vision.describe([(b"img", "image/png")]) == ["ok"]
    assert calls["n"] == 2


async def test_openai_compat_vision_skips_oversized_images(monkeypatch):
    def handler(request: httpx.Request) -> httpx.Response:  # pragma: no cover
        raise AssertionError("oversized image must not be sent")

    _patch_async_client(monkeypatch, handler)
    vision = OpenAICompatVision.from_config({"max_image_bytes": 4, "api_key_env": "NOPE_UNSET"})
    assert await vision.describe([(b"too-big", "image/png")]) == [""]


async def test_anthropic_vision_request_shape(monkeypatch):
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["url"] = str(request.url)
        seen["headers"] = dict(request.headers)
        seen["payload"] = json.loads(request.content)
        return httpx.Response(
            200, json={"content": [{"type": "text", "text": "A flowchart."}]}
        )

    _patch_async_client(monkeypatch, handler)
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-test")

    vision = AnthropicVision.from_config({})
    out = await vision.describe([(b"img", "image/png")])

    assert out == ["A flowchart."]
    assert seen["url"] == "https://api.anthropic.com/v1/messages"
    assert seen["headers"]["x-api-key"] == "sk-test"
    assert seen["headers"]["anthropic-version"] == "2023-06-01"
    source = seen["payload"]["messages"][0]["content"][0]["source"]
    assert source == {
        "type": "base64",
        "media_type": "image/png",
        "data": base64.b64encode(b"img").decode(),
    }


async def test_anthropic_vision_requires_key(monkeypatch):
    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
    vision = AnthropicVision.from_config({})
    with pytest.raises(RuntimeError, match="ANTHROPIC_API_KEY"):
        await vision.describe([(b"img", "image/png")])


async def _no_sleep(_seconds):
    return None


# ---------------------------------------------------------------------------
# Indexer vision step
# ---------------------------------------------------------------------------


def _image_block(desc_target: bytes = b"img") -> Block:
    return Block(
        type="image",
        text="",
        metadata={
            "image_b64": base64.b64encode(desc_target).decode(),
            "image_mime": "image/png",
            "page": 5,
        },
    )


class FakeVision:
    def __init__(self, descriptions):
        self.descriptions = descriptions
        self.received = None

    async def describe(self, images):
        self.received = images
        return self.descriptions


def _fake_store_and_doc():
    from types import SimpleNamespace

    store = SimpleNamespace(blobs={})
    store.put = lambda key, data: store.blobs.__setitem__(key, data)
    store.delete = lambda key: store.blobs.pop(key, None)
    doc = SimpleNamespace(
        id=uuid.UUID("00000000-0000-0000-0000-00000000000d"),
        collection_id=uuid.UUID("00000000-0000-0000-0000-00000000000c"),
        content_hash="abc123def4567890",
    )
    return store, doc


async def test_describe_images_replaces_blocks_and_strips_bytes(monkeypatch):
    fake = FakeVision(["a system diagram"])
    monkeypatch.setattr(
        "rag_core.pipeline.indexer.plugins.create", lambda kind, name, config: fake
    )
    parsed = ParsedDocument(blocks=[Block(text="before"), _image_block(), Block(text="after")])
    pipeline = PipelineConfig(vision_llm=PluginRef(name="whatever"))
    store, doc = _fake_store_and_doc()

    result = await _describe_images(parsed, pipeline, store, doc)

    assert fake.received == [(b"img", "image/png")]
    texts = [(b.type, b.text) for b in result.blocks]
    assert texts == [
        ("paragraph", "before"),
        ("paragraph", "[Figure: a system diagram]"),
        ("paragraph", "after"),
    ]
    figure = result.blocks[1]
    assert "image_b64" not in figure.metadata
    assert figure.metadata["page"] == 5
    assert figure.metadata["figure"]["key"] in store.blobs


async def test_describe_images_drops_blocks_without_vision_plugin():
    parsed = ParsedDocument(blocks=[Block(text="kept"), _image_block()])
    store, doc = _fake_store_and_doc()
    result = await _describe_images(parsed, PipelineConfig(), store, doc)
    assert [b.text for b in result.blocks] == ["kept"]
    assert store.blobs == {}


async def test_describe_images_drops_empty_descriptions(monkeypatch):
    fake = FakeVision(["  "])
    monkeypatch.setattr(
        "rag_core.pipeline.indexer.plugins.create", lambda kind, name, config: fake
    )
    parsed = ParsedDocument(blocks=[_image_block()])
    store, doc = _fake_store_and_doc()
    result = await _describe_images(
        parsed, PipelineConfig(vision_llm=PluginRef(name="x")), store, doc
    )
    assert result.blocks == []
    assert store.blobs == {}


# ---------------------------------------------------------------------------
# Parser auto-selection priority
# ---------------------------------------------------------------------------


def test_docling_wins_pdf_auto_selection():
    from rag_core import plugins
    from rag_core.builtin.parsers.docling import DoclingParser

    parser = plugins.select_parser("application/pdf")
    assert isinstance(parser, DoclingParser)
