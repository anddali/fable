from __future__ import annotations

import asyncio
import os
from typing import ClassVar

import httpx
from pydantic import BaseModel

from rag_core.interfaces import EmbedderPlugin


class OpenAIEmbedderConfig(BaseModel):
    model: str = "text-embedding-3-small"
    dim: int = 1536
    # OpenAI-compatible /embeddings endpoint base. For Azure OpenAI use e.g.
    # https://<resource>.openai.azure.com/openai/v1 (the v1-compatible surface).
    base_url: str = "https://api.openai.com/v1"
    # Name of the environment variable holding the API key (never the key itself).
    api_key_env: str = "OPENAI_API_KEY"
    batch_size: int = 64
    timeout_seconds: float = 60.0
    max_retries: int = 3


class OpenAIEmbedder(EmbedderPlugin):
    """OpenAI-compatible embeddings API (OpenAI, Azure OpenAI v1, vLLM, ...)."""

    name: ClassVar[str] = "openai"
    config_model: ClassVar[type[BaseModel]] = OpenAIEmbedderConfig

    @property
    def dim(self) -> int:
        return self.config.dim  # type: ignore[attr-defined]

    async def embed(self, texts: list[str]) -> list[list[float]]:
        cfg: OpenAIEmbedderConfig = self.config  # type: ignore[assignment]
        api_key = os.environ.get(cfg.api_key_env, "")
        if not api_key:
            raise RuntimeError(f"Environment variable {cfg.api_key_env!r} is not set")

        out: list[list[float]] = []
        async with httpx.AsyncClient(
            base_url=cfg.base_url,
            headers={"Authorization": f"Bearer {api_key}"},
            timeout=cfg.timeout_seconds,
        ) as client:
            for start in range(0, len(texts), cfg.batch_size):
                batch = texts[start : start + cfg.batch_size]
                out.extend(await self._embed_batch(client, cfg, batch))
        return out

    async def _embed_batch(
        self, client: httpx.AsyncClient, cfg: OpenAIEmbedderConfig, batch: list[str]
    ) -> list[list[float]]:
        payload = {"model": cfg.model, "input": batch, "dimensions": cfg.dim}
        last_error: Exception | None = None
        for attempt in range(cfg.max_retries + 1):
            try:
                resp = await client.post("/embeddings", json=payload)
                if resp.status_code in (429, 500, 502, 503, 504):
                    raise httpx.HTTPStatusError(
                        f"retryable status {resp.status_code}",
                        request=resp.request,
                        response=resp,
                    )
                resp.raise_for_status()
                data = sorted(resp.json()["data"], key=lambda d: d["index"])
                return [d["embedding"] for d in data]
            except (httpx.TransportError, httpx.HTTPStatusError) as exc:
                if (
                    isinstance(exc, httpx.HTTPStatusError)
                    and exc.response is not None
                    and exc.response.status_code not in (429, 500, 502, 503, 504)
                ):
                    raise
                last_error = exc
                if attempt < cfg.max_retries:
                    await asyncio.sleep(2**attempt)
        raise RuntimeError(f"Embedding request failed after retries: {last_error}")
