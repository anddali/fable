"""Vision descriptions via any OpenAI-compatible chat completions API.

Covers OpenAI itself, Ollama (e.g. ``gemma4`` at ``http://host:11434/v1``),
vLLM and Azure OpenAI's v1 surface. The API key is optional because local
servers like Ollama don't require one.
"""

from __future__ import annotations

import asyncio
import base64
import os
from typing import ClassVar

import httpx
from pydantic import BaseModel

from rag_core.interfaces import VisionPlugin

DEFAULT_PROMPT = (
    "Describe this image for a document search index. If it is a diagram, "
    "chart or screenshot, explain what it shows, including labels, "
    "relationships and any data it conveys. Be factual and concise."
)

_RETRYABLE = (429, 500, 502, 503, 504)


class OpenAICompatVisionConfig(BaseModel):
    model: str = "gpt-4o-mini"
    # OpenAI-compatible /chat/completions endpoint base.
    base_url: str = "https://api.openai.com/v1"
    # Name of the environment variable holding the API key. The variable may
    # be unset/empty for servers that don't authenticate (e.g. local Ollama).
    api_key_env: str = "OPENAI_API_KEY"
    prompt: str = DEFAULT_PROMPT
    max_tokens: int = 500
    # Images larger than this are skipped (empty description) rather than
    # shipped to the model.
    max_image_bytes: int = 20 * 1024 * 1024
    timeout_seconds: float = 120.0
    max_retries: int = 3


class OpenAICompatVision(VisionPlugin):
    name: ClassVar[str] = "openai-compat"
    config_model: ClassVar[type[BaseModel]] = OpenAICompatVisionConfig

    async def describe(self, images: list[tuple[bytes, str]]) -> list[str]:
        cfg: OpenAICompatVisionConfig = self.config  # type: ignore[assignment]
        headers = {}
        api_key = os.environ.get(cfg.api_key_env, "")
        if api_key:
            headers["Authorization"] = f"Bearer {api_key}"

        out: list[str] = []
        async with httpx.AsyncClient(
            base_url=cfg.base_url, headers=headers, timeout=cfg.timeout_seconds
        ) as client:
            for data, mime in images:
                if len(data) > cfg.max_image_bytes:
                    out.append("")
                    continue
                out.append(await self._describe_one(client, cfg, data, mime))
        return out

    async def _describe_one(
        self, client: httpx.AsyncClient, cfg: OpenAICompatVisionConfig, data: bytes, mime: str
    ) -> str:
        data_uri = f"data:{mime};base64,{base64.b64encode(data).decode('ascii')}"
        payload = {
            "model": cfg.model,
            "max_tokens": cfg.max_tokens,
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": cfg.prompt},
                        {"type": "image_url", "image_url": {"url": data_uri}},
                    ],
                }
            ],
        }
        last_error: Exception | None = None
        for attempt in range(cfg.max_retries + 1):
            try:
                resp = await client.post("/chat/completions", json=payload)
                if resp.status_code in _RETRYABLE:
                    raise httpx.HTTPStatusError(
                        f"retryable status {resp.status_code}",
                        request=resp.request,
                        response=resp,
                    )
                resp.raise_for_status()
                content = resp.json()["choices"][0]["message"]["content"]
                return (content or "").strip()
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
