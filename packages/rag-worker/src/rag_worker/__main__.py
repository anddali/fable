"""Worker entrypoint: ``rag-worker`` or ``python -m rag_worker``."""

from __future__ import annotations

import asyncio
import sys

from rag_core.config import get_settings
from rag_core.logging import configure_logging
from rag_core.queue import QUEUE_BEAT, QUEUE_INDEX, QUEUE_SYNC


def main() -> None:
    settings = get_settings()
    configure_logging(settings.log_level)

    # Importing tasks registers them on the shared app.
    from rag_worker.tasks import app

    async def _run() -> None:
        async with app.open_async():
            await app.run_worker_async(
                queues=[QUEUE_SYNC, QUEUE_INDEX, QUEUE_BEAT],
                concurrency=4,
                install_signal_handlers=True,
            )

    if sys.platform == "win32":
        # psycopg async needs a selector loop; win32 defaults to proactor.
        with asyncio.Runner(loop_factory=asyncio.SelectorEventLoop) as runner:
            runner.run(_run())
    else:
        asyncio.run(_run())


if __name__ == "__main__":
    main()
