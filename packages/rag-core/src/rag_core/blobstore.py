"""Raw-bytes storage for source payloads and manual uploads.

The indexing pipeline never re-fetches from the remote system: sync writes the
payload here once, and (re)indexing reads it back. Local filesystem is the
default; implement BlobStore against S3/Azure Blob for multi-node deployments.
"""

from __future__ import annotations

from abc import ABC, abstractmethod
from pathlib import Path

_SAFE = set("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-")


def _sanitize(part: str) -> str:
    return "".join(c if c in _SAFE else "_" for c in part) or "_"


class BlobStore(ABC):
    @abstractmethod
    def put(self, key: str, data: bytes) -> None: ...

    @abstractmethod
    def get(self, key: str) -> bytes: ...

    @abstractmethod
    def delete(self, key: str) -> None: ...

    @abstractmethod
    def exists(self, key: str) -> bool: ...


class LocalBlobStore(BlobStore):
    def __init__(self, root: str | Path) -> None:
        self.root = Path(root)
        self.root.mkdir(parents=True, exist_ok=True)

    def _path(self, key: str) -> Path:
        parts = [_sanitize(p) for p in key.split("/") if p]
        if not parts:
            raise ValueError("empty blob key")
        return self.root.joinpath(*parts)

    def put(self, key: str, data: bytes) -> None:
        path = self._path(key)
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp = path.with_suffix(path.suffix + ".tmp")
        tmp.write_bytes(data)
        tmp.replace(path)

    def get(self, key: str) -> bytes:
        return self._path(key).read_bytes()

    def delete(self, key: str) -> None:
        self._path(key).unlink(missing_ok=True)

    def exists(self, key: str) -> bool:
        return self._path(key).exists()


def document_blob_key(collection_id, document_id, content_hash: str) -> str:
    return f"{collection_id}/{document_id}/{content_hash}"
