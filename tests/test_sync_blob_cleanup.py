"""Sync engine blob hygiene: superseded payloads must not orphan."""

import hashlib
import uuid
from types import SimpleNamespace

from rag_core.blobstore import LocalBlobStore, document_blob_key
from rag_core.schemas import RawDocument
from rag_worker import sync as sync_mod


class FakeResult:
    def __init__(self, items):
        self._items = items

    def scalars(self):
        return self._items


class FakeSession:
    def __init__(self, docs):
        self._docs = docs

    async def execute(self, *_args, **_kwargs):
        return FakeResult(self._docs)

    async def flush(self):
        pass


class StubConnector:
    incremental = False

    def __init__(self, raws):
        self._raws = raws

    async def iter_documents(self, _ctx):
        for raw in self._raws:
            yield raw


def setup_existing(store: LocalBlobStore, payload: bytes):
    coll_id, doc_id = uuid.uuid4(), uuid.uuid4()
    content_hash = hashlib.sha256(payload).hexdigest()
    blob_key = document_blob_key(coll_id, doc_id, content_hash)
    store.put(blob_key, payload)
    doc = SimpleNamespace(
        id=doc_id,
        external_id="page:1",
        content_hash=content_hash,
        status="indexed",
        blob_key=blob_key,
        title="t",
    )
    source = SimpleNamespace(
        id=uuid.uuid4(), collection_id=coll_id, connector="stub", config={}, state={}
    )
    return doc, source, blob_key


async def run_sync(monkeypatch, store, doc, source, raws):
    monkeypatch.setattr(sync_mod.plugins, "create", lambda *a, **k: StubConnector(raws))

    async def noop_defer(*_args, **_kwargs):
        pass

    monkeypatch.setattr(sync_mod, "defer_index", noop_defer)
    return await sync_mod._sync_source(FakeSession([doc]), store, source)


async def test_superseded_blob_is_deleted_on_content_change(tmp_path, monkeypatch):
    store = LocalBlobStore(tmp_path)
    doc, source, old_key = setup_existing(store, b"version 1")

    counts = await run_sync(
        monkeypatch, store, doc, source,
        [RawDocument(external_id="page:1", title="t", text="version 2")],
    )

    assert counts["updated"] == 1
    assert doc.blob_key != old_key
    assert store.exists(doc.blob_key)  # new payload persisted
    assert not store.exists(old_key)  # superseded payload removed


async def test_unchanged_document_keeps_blob(tmp_path, monkeypatch):
    store = LocalBlobStore(tmp_path)
    doc, source, old_key = setup_existing(store, b"version 1")

    counts = await run_sync(
        monkeypatch, store, doc, source,
        [RawDocument(external_id="page:1", title="t", text="version 1")],
    )

    assert counts["unchanged"] == 1
    assert doc.blob_key == old_key
    assert store.exists(old_key)
