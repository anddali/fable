"""Collection deletion: ORM cascade config + blob-store purge.

Two failure modes are guarded here:
  1. A bare ``sources``/``documents`` relationship makes SQLAlchemy try to NULL
     the children's NOT NULL FK on parent delete -> IntegrityError, so the
     collection never deletes. The cascade must defer to the DB ON DELETE.
  2. The DB cascade removes rows only; document payloads and figure images
     would orphan on disk unless delete_collection purges them.
"""

import uuid
from types import SimpleNamespace

from rag_core.db.models import Collection, Source
from rag_core.pipeline.indexer import collection_blob_keys

# ---------------------------------------------------------------------------
# 1. Relationship cascade configuration
# ---------------------------------------------------------------------------


def test_collection_sources_defer_to_db_cascade():
    rel = Collection.__mapper__.relationships["sources"]
    assert rel.cascade.delete_orphan  # ORM-side delete, not FK nullify
    assert rel.passive_deletes is True  # let DB ON DELETE CASCADE do the work


def test_source_documents_defer_to_db_cascade():
    rel = Source.__mapper__.relationships["documents"]
    assert rel.cascade.delete_orphan
    assert rel.passive_deletes is True


# ---------------------------------------------------------------------------
# 2. Blob-key collection + purge ordering
# ---------------------------------------------------------------------------


class FakeResult:
    def __init__(self, rows):
        self._rows = rows

    def __iter__(self):
        return iter(self._rows)


class FakeSession:
    """Returns queued results for collection_blob_keys' two SELECTs in order."""

    def __init__(self, results):
        self._results = list(results)
        self.deleted = []
        self.committed = False

    async def execute(self, *_args, **_kwargs):
        return self._results.pop(0)

    async def delete(self, obj):
        self.deleted.append(obj)

    async def commit(self):
        self.committed = True


class MemoryBlobStore:
    def __init__(self, blobs):
        self.blobs = dict(blobs)

    def delete(self, key):
        self.blobs.pop(key, None)


def _session_with(payload_keys, figure_metas, upload_keys=()):
    # collection_blob_keys runs three SELECTs in order: payloads, figures, uploads.
    return FakeSession(
        [
            FakeResult([(k,) for k in payload_keys]),
            FakeResult([(m,) for m in figure_metas]),
            FakeResult([(k,) for k in upload_keys]),
        ]
    )


async def test_collection_blob_keys_unions_payloads_figures_and_uploads():
    session = _session_with(
        payload_keys=["c/d1/hash1", "c/d2/hash2", None],
        figure_metas=[
            {"figures": [{"key": "c/d1/figures/aa-0.png"}, {"key": "c/d1/figures/aa-1.png"}]},
            {"figures": [{"page": 2}]},  # figure ref without a key -> skipped
            {},
            None,
        ],
        upload_keys=["uploads/s1/i1/hash1", None],
    )
    keys = await collection_blob_keys(session, uuid.uuid4())
    assert keys == {
        "c/d1/hash1",
        "c/d2/hash2",
        "c/d1/figures/aa-0.png",
        "c/d1/figures/aa-1.png",
        "uploads/s1/i1/hash1",
    }


async def test_delete_collection_purges_blobs_after_commit():
    from rag_api.routers.collections import delete_collection

    store = MemoryBlobStore(
        {
            "c/d1/hash1": b"payload",
            "c/d1/figures/aa-0.png": b"png",
            "uploads/s1/i1/hash1": b"staged",
            "other/keep": b"x",
        }
    )
    session = _session_with(
        payload_keys=["c/d1/hash1"],
        figure_metas=[{"figures": [{"key": "c/d1/figures/aa-0.png"}]}],
        upload_keys=["uploads/s1/i1/hash1"],
    )
    collection = SimpleNamespace(id=uuid.uuid4())

    await delete_collection(session=session, blob_store=store, collection=collection)

    assert session.committed is True
    assert collection in session.deleted
    # Collection's blobs gone; unrelated blob untouched.
    assert store.blobs == {"other/keep": b"x"}
