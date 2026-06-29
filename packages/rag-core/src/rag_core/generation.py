"""Chat-completion calls against any OpenAI-compatible ``/chat/completions`` API.

Backs the console Chat endpoint: given a list of role/content messages, return
the assistant's reply text. Same provider surface and retry posture as the
``openai-compat`` vision plugin (OpenAI, Ollama, vLLM, Azure v1). Non-streaming
to keep the API response a single JSON body.
"""

from __future__ import annotations

import asyncio
import os

import httpx

from rag_core.config import ChatSettings

_RETRYABLE = (429, 500, 502, 503, 504)
_MAX_RETRIES = 2


class ChatLLMError(RuntimeError):
    """The chat model could not be reached or returned an unusable response."""


async def chat_completion(cfg: ChatSettings, messages: list[dict]) -> str:
    """Send ``messages`` to the configured chat model and return the reply text."""
    headers = {}
    api_key = os.environ.get(cfg.api_key_env, "")
    if api_key:
        headers["Authorization"] = f"Bearer {api_key}"
    payload = {
        "model": cfg.model,
        "messages": messages,
        "temperature": cfg.temperature,
        "max_tokens": cfg.max_tokens,
        "stream": False,
    }

    last_error: Exception | None = None
    async with httpx.AsyncClient(
        base_url=cfg.base_url, headers=headers, timeout=cfg.timeout_seconds
    ) as client:
        for attempt in range(_MAX_RETRIES + 1):
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
                text = (content or "").strip()
                if not text:
                    raise ChatLLMError("chat model returned an empty response")
                return text
            except (httpx.TransportError, httpx.HTTPStatusError) as exc:
                if (
                    isinstance(exc, httpx.HTTPStatusError)
                    and exc.response is not None
                    and exc.response.status_code not in _RETRYABLE
                ):
                    raise ChatLLMError(f"chat model request failed: {exc}") from exc
                last_error = exc
                if attempt < _MAX_RETRIES:
                    await asyncio.sleep(2**attempt)
            except (KeyError, IndexError, ValueError) as exc:
                raise ChatLLMError(f"unexpected chat model response: {exc}") from exc
    raise ChatLLMError(f"chat model unreachable after retries: {last_error}")
