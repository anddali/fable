from __future__ import annotations

import hashlib
import math
import struct
from typing import ClassVar

from pydantic import BaseModel

from rag_core.interfaces import EmbedderPlugin


class FakeEmbedderConfig(BaseModel):
    dim: int = 1536


class FakeEmbedder(EmbedderPlugin):
    """Deterministic hash-based vectors for local development and tests.

    No external calls; identical text always embeds identically. Useless for
    semantic quality — never use outside dev/test environments.
    """

    name: ClassVar[str] = "fake"
    config_model: ClassVar[type[BaseModel]] = FakeEmbedderConfig

    @property
    def dim(self) -> int:
        return self.config.dim  # type: ignore[attr-defined]

    async def embed(self, texts: list[str]) -> list[list[float]]:
        return [self._one(t) for t in texts]

    def _one(self, text: str) -> list[float]:
        values: list[float] = []
        counter = 0
        seed = text.encode("utf-8")
        while len(values) < self.dim:
            digest = hashlib.sha256(seed + struct.pack("<I", counter)).digest()
            values.extend(
                struct.unpack("<i", digest[i : i + 4])[0] / 2**31 for i in range(0, 32, 4)
            )
            counter += 1
        vec = values[: self.dim]
        norm = math.sqrt(sum(v * v for v in vec)) or 1.0
        return [v / norm for v in vec]
