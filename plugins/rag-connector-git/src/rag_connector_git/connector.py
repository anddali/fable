"""Git connector: shallow-clones (or fetches) a repo into the cache dir and
ingests files matching the configured globs.

Requires the ``git`` binary in the runtime image. Private repos: put a token
in the env var named by ``token_env``; it is injected into the HTTPS URL at
clone time only and never persisted.
"""

from __future__ import annotations

import asyncio
import fnmatch
import hashlib
import os
from collections.abc import AsyncIterator
from pathlib import Path
from typing import ClassVar
from urllib.parse import urlsplit, urlunsplit

from pydantic import BaseModel, Field

from rag_core.interfaces import ConnectorPlugin, SyncContext
from rag_core.schemas import RawDocument

_EXT_MIME = {
    ".md": "text/markdown",
    ".markdown": "text/markdown",
    ".txt": "text/plain",
    ".rst": "text/plain",
    ".csv": "text/csv",
    ".pdf": "application/pdf",
}


class GitConfig(BaseModel):
    repo_url: str  # https URL
    branch: str = "main"
    include_globs: list[str] = Field(default_factory=lambda: ["**/*.md", "docs/**/*"])
    exclude_globs: list[str] = Field(default_factory=lambda: ["**/node_modules/**", "**/.git/**"])
    # Environment variable holding an access token for private repos ("" = public).
    token_env: str = ""
    max_file_bytes: int = 10 * 1024 * 1024


async def _run_git(*args: str, cwd: Path | None = None) -> str:
    proc = await asyncio.create_subprocess_exec(
        "git",
        *args,
        cwd=cwd,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
        env={**os.environ, "GIT_TERMINAL_PROMPT": "0"},
    )
    stdout, stderr = await proc.communicate()
    if proc.returncode != 0:
        raise RuntimeError(
            f"git {' '.join(args[:2])} failed: {stderr.decode(errors='replace')[:500]}"
        )
    return stdout.decode(errors="replace")


class GitConnector(ConnectorPlugin):
    name: ClassVar[str] = "git"
    config_model: ClassVar[type[BaseModel]] = GitConfig

    async def iter_documents(self, ctx: SyncContext) -> AsyncIterator[RawDocument]:
        cfg: GitConfig = self.config  # type: ignore[assignment]
        repo_dir = (
            Path(ctx.settings.cache_dir)
            / "git"
            / hashlib.sha256(f"{ctx.source_id}".encode()).hexdigest()[:16]
        )
        await self._clone_or_update(cfg, repo_dir)

        head = (await _run_git("rev-parse", "HEAD", cwd=repo_dir)).strip()
        ctx.state["head_commit"] = head

        for path in sorted(repo_dir.rglob("*")):
            if not path.is_file():
                continue
            rel = path.relative_to(repo_dir).as_posix()
            if rel.startswith(".git/"):
                continue
            if not any(fnmatch.fnmatch(rel, g) for g in cfg.include_globs):
                continue
            if any(fnmatch.fnmatch(rel, g) for g in cfg.exclude_globs):
                continue
            if path.stat().st_size > cfg.max_file_bytes:
                continue
            ext = path.suffix.lower()
            yield RawDocument(
                external_id=rel,
                title=path.name,
                uri=f"{cfg.repo_url.removesuffix('.git')}/blob/{cfg.branch}/{rel}",
                mime_type=_EXT_MIME.get(ext, "text/plain"),
                content=path.read_bytes(),
                metadata={"path": rel, "commit": head, "branch": cfg.branch},
            )

    async def _clone_or_update(self, cfg: GitConfig, repo_dir: Path) -> None:
        url = self._authed_url(cfg)
        if (repo_dir / ".git").exists():
            await _run_git("remote", "set-url", "origin", url, cwd=repo_dir)
            await _run_git("fetch", "--depth", "1", "origin", cfg.branch, cwd=repo_dir)
            await _run_git("checkout", "-f", "FETCH_HEAD", cwd=repo_dir)
        else:
            repo_dir.parent.mkdir(parents=True, exist_ok=True)
            await _run_git("clone", "--depth", "1", "--branch", cfg.branch, url, str(repo_dir))
        # Scrub credentials from the stored remote.
        await _run_git("remote", "set-url", "origin", cfg.repo_url, cwd=repo_dir)

    def _authed_url(self, cfg: GitConfig) -> str:
        if not cfg.token_env:
            return cfg.repo_url
        token = os.environ.get(cfg.token_env, "")
        if not token:
            raise RuntimeError(f"Environment variable {cfg.token_env!r} is not set")
        parts = urlsplit(cfg.repo_url)
        return urlunsplit(parts._replace(netloc=f"x-access-token:{token}@{parts.netloc}"))
