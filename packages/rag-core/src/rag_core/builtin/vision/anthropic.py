"""Vision descriptions via the Anthropic Messages API (Claude models)."""

from __future__ import annotations

import asyncio
import base64
import os
from typing import ClassVar

import httpx
from pydantic import BaseModel

from rag_core.interfaces import VisionPlugin

from .openai_compat import DEFAULT_PROMPT, _RETRYABLE


class AnthropicVisionConfig(BaseModel):
    model: str = "claude-haiku-4-5-20251001"
    base_url: str = "https://api.anthropic.com"
    api_key_env: str = "ANTHROPIC_API_KEY"
    prompt: str = DEFAULT_PROMPT
    max_tokens: int = 500
    max_image_bytes: int = 5 * 1024 * 1024  # Anthropic's per-image API limit
    timeout_seconds: float = 120.0
    max_retries: int = 3


class AnthropicVision(VisionPlugin):
    name: ClassVar[str] = "anthropic"
    config_model: ClassVar[type[BaseModel]] = AnthropicVisionConfig

    async def describe(self, images: list[tuple[bytes, str]]) -> list[str]:
        cfg: AnthropicVisionConfig = self.config  # type: ignore[assignment]
        api_key = os.environ.get(cfg.api_key_env, "")
        if not api_key:
            raise RuntimeError(f"Environment variable {cfg.api_key_env!r} is not set")

        out: list[str] = []
        async with httpx.AsyncClient(
            base_url=cfg.base_url,
            headers={"x-api-key": api_key, "anthropic-version": "2023-06-01"},
            timeout=cfg.timeout_seconds,
        ) as client:
            for data, mime in images:
                if len(data) > cfg.max_image_bytes:
                    out.append("")
                    continue
                out.append(await self._describe_one(client, cfg, data, mime))
        return out

    async def _describe_one(
        self, client: httpx.AsyncClient, cfg: AnthropicVisionConfig, data: bytes, mime: str
    ) -> str:
        payload = {
            "model": cfg.model,
            "max_tokens": cfg.max_tokens,
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": mime,
                                "data": base64.b64encode(data).decode("ascii"),
                            },
                        },
                        {"type": "text", "text": cfg.prompt},
                    ],
                }
            ],
        }
        last_error: Exception | None = None
        for attempt in range(cfg.max_retries + 1):
            try:
                resp = await client.post("/v1/messages", json=payload)
                if resp.status_code in _RETRYABLE:
                    raise httpx.HTTPStatusError(
                        f"retryable status {resp.status_code}",
                        request=resp.request,
                        response=resp,
                    )
                resp.raise_for_status()
                parts = resp.json()["content"]
                text = "".join(p.get("text", "") for p in parts if p.get("type") == "text")
                return text.strip()
            except (httpx.TransportError, httpx.HTTPStatusError) as exc:
                if (
                    isinstance(exc, httpx.HTTPStatusError)
                    and exc.response is not None
                    and exc.response.status_code not in _RETRYABLE
                ):
                    raise
                last_error = exc
                if attempt < cfg.max_retries:
                    await asyncio.sleep(2**attempt)
        raise RuntimeError(f"Vision request failed after retries: {last_error}")
