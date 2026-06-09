"""Worker entrypoint: ``rag-worker`` or ``python -m rag_worker``."""

from __future__ import annotations

import asyncio

from rag_core.config import get_settings
from rag_core.logging import configure_logging
from rag_core.queue import QUEUE_INDEX, QUEUE_SYNC


def main() -> None:
    settings = get_settings()
    configure_logging(settings.log_level)

    # Importing tasks registers them on the shared app.
    from rag_worker.tasks import app

    async def _run() -> None:
        async with app.open_async():
            await app.run_worker_async(
                queues=[QUEUE_SYNC, QUEUE_INDEX],
                concurrency=4,
                install_signal_handlers=True,
            )

    asyncio.run(_run())


if __name__ == "__main__":
    main()
