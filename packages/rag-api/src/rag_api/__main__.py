"""Dev entrypoint: ``rag-api`` (or ``python -m rag_api``).

Exists mainly for Windows development: psycopg's async pool requires a
selector event loop, but Python defaults to the proactor loop on win32 (and
uvicorn's CLI manages its own loop), so a bare ``uvicorn rag_api.main:app``
cannot reach Postgres there. Production (Linux containers) runs plain uvicorn.
"""

from __future__ import annotations

import argparse
import asyncio
import sys

import uvicorn


def main() -> None:
    parser = argparse.ArgumentParser(prog="rag-api")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8000)
    args = parser.parse_args()

    server = uvicorn.Server(uvicorn.Config("rag_api.main:app", host=args.host, port=args.port))
    if sys.platform == "win32":
        with asyncio.Runner(loop_factory=asyncio.SelectorEventLoop) as runner:
            runner.run(server.serve())
    else:
        asyncio.run(server.serve())


if __name__ == "__main__":
    main()
