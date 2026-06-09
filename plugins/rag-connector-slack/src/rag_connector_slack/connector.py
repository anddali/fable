"""Slack connector: ingests channel history as one document per channel-day.

Day-bucketing gives stable external ids (re-syncing an unchanged day is a
no-op via content hashing) and retrieval-sized documents. Requires a bot token
with ``channels:history``/``channels:read`` (and the bot invited to each
channel).
"""

from __future__ import annotations

import os
from collections import defaultdict
from collections.abc import AsyncIterator
from datetime import UTC, datetime, timedelta
from typing import ClassVar

import httpx
from pydantic import BaseModel, Field

from rag_core.interfaces import ConnectorPlugin, SyncContext
from rag_core.schemas import RawDocument

SLACK_API = "https://slack.com/api"


class SlackConfig(BaseModel):
    channel_ids: list[str] = Field(min_length=1)
    # Environment variable holding the bot token (xoxb-...).
    bot_token_env: str = "SLACK_BOT_TOKEN"
    # How far back to ingest on every run (full enumeration keeps deletions
    # and edits consistent; old days stop being yielded and are tombstoned).
    lookback_days: int = 90
    include_threads: bool = True


class SlackConnector(ConnectorPlugin):
    name: ClassVar[str] = "slack"
    config_model: ClassVar[type[BaseModel]] = SlackConfig

    async def iter_documents(self, ctx: SyncContext) -> AsyncIterator[RawDocument]:
        cfg: SlackConfig = self.config  # type: ignore[assignment]
        token = os.environ.get(cfg.bot_token_env, "")
        if not token:
            raise RuntimeError(f"Environment variable {cfg.bot_token_env!r} is not set")

        oldest = (datetime.now(UTC) - timedelta(days=cfg.lookback_days)).timestamp()
        async with httpx.AsyncClient(
            base_url=SLACK_API, headers={"Authorization": f"Bearer {token}"}, timeout=60
        ) as client:
            for channel_id in cfg.channel_ids:
                channel_name = await self._channel_name(client, channel_id)
                messages = await self._fetch_history(client, cfg, channel_id, oldest)
                by_day: dict[str, list[dict]] = defaultdict(list)
                for msg in messages:
                    day = datetime.fromtimestamp(float(msg["ts"]), tz=UTC).strftime("%Y-%m-%d")
                    by_day[day].append(msg)

                for day, msgs in sorted(by_day.items()):
                    lines = [
                        f"[{datetime.fromtimestamp(float(m['ts']), tz=UTC):%H:%M}] "
                        f"{m.get('user', m.get('bot_id', 'unknown'))}: {m.get('text', '')}"
                        for m in sorted(msgs, key=lambda m: float(m["ts"]))
                        if m.get("text")
                    ]
                    if not lines:
                        continue
                    yield RawDocument(
                        external_id=f"{channel_id}:{day}",
                        title=f"#{channel_name} — {day}",
                        uri=f"https://slack.com/app_redirect?channel={channel_id}",
                        mime_type="text/plain",
                        text="\n".join(lines),
                        metadata={"channel": channel_name, "channel_id": channel_id, "day": day},
                    )

    async def _channel_name(self, client: httpx.AsyncClient, channel_id: str) -> str:
        body = await self._call(client, "conversations.info", {"channel": channel_id})
        return body.get("channel", {}).get("name", channel_id)

    async def _fetch_history(
        self, client: httpx.AsyncClient, cfg: SlackConfig, channel_id: str, oldest: float
    ) -> list[dict]:
        messages: list[dict] = []
        cursor = ""
        while True:
            params = {"channel": channel_id, "oldest": f"{oldest:.6f}", "limit": 200}
            if cursor:
                params["cursor"] = cursor
            body = await self._call(client, "conversations.history", params)
            page = body.get("messages", [])
            messages.extend(m for m in page if m.get("type") == "message")
            if cfg.include_threads:
                for msg in page:
                    if msg.get("reply_count") and msg.get("thread_ts") == msg.get("ts"):
                        messages.extend(
                            await self._fetch_replies(client, channel_id, msg["thread_ts"])
                        )
            cursor = (body.get("response_metadata") or {}).get("next_cursor", "")
            if not cursor:
                return messages

    async def _fetch_replies(
        self, client: httpx.AsyncClient, channel_id: str, thread_ts: str
    ) -> list[dict]:
        body = await self._call(
            client, "conversations.replies", {"channel": channel_id, "ts": thread_ts, "limit": 200}
        )
        # First entry is the parent message, already collected.
        return [m for m in body.get("messages", [])[1:] if m.get("type") == "message"]

    async def _call(self, client: httpx.AsyncClient, method: str, params: dict) -> dict:
        resp = await client.get(f"/{method}", params=params)
        resp.raise_for_status()
        body = resp.json()
        if not body.get("ok"):
            raise RuntimeError(f"Slack API {method} failed: {body.get('error', 'unknown')}")
        return body
