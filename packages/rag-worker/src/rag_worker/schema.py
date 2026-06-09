"""Applies the procrastinate queue schema (``rag-apply-schema``).

Run once per environment after ``alembic upgrade head``; re-running is a no-op
(procrastinate's DDL itself is not idempotent, so we check first).
"""

from __future__ import annotations

import asyncio

from rag_core.queue import get_app


async def _apply() -> None:
    app = get_app()
    async with app.open_async():
        already = await app.connector.execute_query_one_async(
            "SELECT to_regclass('procrastinate_jobs') IS NOT NULL AS applied"
        )
        if already["applied"]:
            print("procrastinate schema already present; skipping")
            return
        await app.schema_manager.apply_schema_async()
        print("procrastinate schema applied")


def main() -> None:
    asyncio.run(_apply())


if __name__ == "__main__":
    main()
