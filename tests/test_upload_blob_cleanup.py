"""Manual-upload blob hygiene: deleting a file removes its staging blob, and
the local store doesn't leave empty folders behind after deletes."""

import uuid
from types import SimpleNamespace

import pytest
from fastapi import HTTPException

from rag_api.routers.uploads import delete_file
from rag_core.blobstore import LocalBlobStore


class MemoryBlobStore:
    def __init__(self, blobs):
        self.blobs = dict(blobs)

    def delete(self, key):
        self.blobs.pop(key, None)


class FakeSession:
    """get() dispatches by model; records commit."""

    def __init__(self, by_model):
        self._by_model = by_model
        self.committed = False

    async def get(self, model, _id):
        return self._by_model.get(model.__name__)

    async def commit(self):
        self.committed = True


def _setup(deleted=False):
    cid, sid, iid = uuid.uuid4(), uuid.uuid4(), uuid.uuid4()
    source = SimpleNamespace(id=sid, collection_id=cid, connector="upload")
    item = SimpleNamespace(
        id=iid, source_id=sid, deleted=deleted, blob_key=f"uploads/{sid}/{iid}/hash"
    )
    session = FakeSession({"Source": source, "UploadItem": item})
    collection = SimpleNamespace(id=cid)
    return cid, sid, iid, source, item, session, collection


async def test_delete_file_removes_staging_blob():
    cid, sid, iid, source, item, session, collection = _setup()
    store = MemoryBlobStore({item.blob_key: b"staged", "uploads/other": b"keep"})

    await delete_file(
        source_id=sid, item_id=iid, session=session, blob_store=store, collection=collection
    )

    assert item.deleted is True
    assert session.committed is True
    assert store.blobs == {"uploads/other": b"keep"}  # staging blob gone


async def test_delete_file_already_deleted_is_404_and_keeps_blob():
    cid, sid, iid, source, item, session, collection = _setup(deleted=True)
    store = MemoryBlobStore({item.blob_key: b"staged"})

    with pytest.raises(HTTPException) as exc:
        await delete_file(
            source_id=sid, item_id=iid, session=session, blob_store=store, collection=collection
        )
    assert exc.value.status_code == 404
    assert store.blobs == {item.blob_key: b"staged"}  # untouched


def test_local_store_prunes_empty_parent_dirs(tmp_path):
    store = LocalBlobStore(tmp_path)
    store.put("c/d/figures/aa-0.png", b"x")
    store.put("c/d/hash", b"y")

    store.delete("c/d/figures/aa-0.png")
    # figures/ is now empty and should be gone; c/ and c/d still hold the payload.
    assert not (tmp_path / "c" / "d" / "figures").exists()
    assert (tmp_path / "c" / "d" / "hash").exists()

    store.delete("c/d/hash")
    # last blob gone -> the whole c/ subtree collapses, but the root remains.
    assert not (tmp_path / "c").exists()
    assert tmp_path.exists()
