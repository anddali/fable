"""Blob-store garbage collector (``rag-gc-blobs``).

Mark-and-sweep over the local blob store: any file under the blob dir that no
live DB row references is an orphan and gets removed. Reclaims blobs that
predate the delete-time cleanup (superseded payloads, figures of tombstoned
documents, and — the common case — staging blobs of deleted manual uploads).

Defaults to a dry run; pass ``--apply`` to actually delete. Run where both the
DB and the blob volume are reachable, e.g.::

    docker compose exec worker rag-gc-blobs            # report only
    docker compose exec worker rag-gc-blobs --apply    # delete orphans
"""

from __future__ import annotations

import argparse
import asyncio
from pathlib import Path

from sqlalchemy import select

from rag_core.blobstore import LocalBlobStore
from rag_core.config import get_settings
from rag_core.db.models import Chunk, Document, UploadItem
from rag_core.db.session import session_scope


async def _live_keys() -> set[str]:
    """Every blob key still referenced by a row we keep."""
    keys: set[str] = set()
    async with session_scope() as session:
        payloads = await session.execute(select(Document.blob_key))
        keys.update(k for (k,) in payloads if k)

        figures = await session.execute(select(Chunk.chunk_metadata))
        keys.update(
            f["key"]
            for (meta,) in figures
            for f in (meta or {}).get("figures", [])
            if f.get("key")
        )

        # Only non-deleted uploads are live; a deleted item's staging blob is an
        # orphan (the connector no longer surfaces it).
        uploads = await session.execute(
            select(UploadItem.blob_key).where(UploadItem.deleted.is_(False))
        )
        keys.update(k for (k,) in uploads if k)
    return keys


def _prune_empty_dirs(root: Path) -> int:
    """Remove empty directories under root (deepest first); keep root itself."""
    removed = 0
    dirs = sorted(
        (p for p in root.rglob("*") if p.is_dir()),
        key=lambda p: len(p.parts),
        reverse=True,
    )
    for d in dirs:
        try:
            d.rmdir()  # only succeeds when empty
            removed += 1
        except OSError:
            pass
    return removed


def _run(apply: bool) -> None:
    settings = get_settings()
    root = Path(settings.blob_dir)
    if not root.exists():
        print(f"blob dir {root} does not exist; nothing to do")
        return

    store = LocalBlobStore(root)
    live = asyncio.run(_live_keys())  # the only async work is the DB query

    orphans: list[tuple[str, int]] = []
    for path in root.rglob("*"):
        if not path.is_file():
            continue
        key = path.relative_to(root).as_posix()
        if key not in live:
            orphans.append((key, path.stat().st_size))

    total_bytes = sum(size for _, size in orphans)
    print(f"live refs: {len(live)} · orphan files: {len(orphans)} · {total_bytes} bytes")
    for key, size in orphans:
        print(f"  {'delete' if apply else 'orphan'}: {key} ({size} bytes)")

    if not apply:
        print("dry run — re-run with --apply to delete")
        return

    for key, _ in orphans:
        store.delete(key)
    pruned = _prune_empty_dirs(root)
    print(f"deleted {len(orphans)} blobs, freed {total_bytes} bytes, pruned {pruned} empty dirs")


def main() -> None:
    parser = argparse.ArgumentParser(description="Garbage-collect orphaned blobs.")
    parser.add_argument("--apply", action="store_true", help="delete orphans (default: dry run)")
    args = parser.parse_args()
    _run(args.apply)


if __name__ == "__main__":
    main()
