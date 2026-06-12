# Single image for api / worker / migrations; pick the role via the command.
FROM ghcr.io/astral-sh/uv:python3.12-bookworm-slim AS builder

ENV UV_COMPILE_BYTECODE=1 UV_LINK_MODE=copy
WORKDIR /app

# Dependency layer (cached until lockfile or any member pyproject changes).
COPY pyproject.toml uv.lock ./
COPY packages/rag-core/pyproject.toml packages/rag-core/pyproject.toml
COPY packages/rag-api/pyproject.toml packages/rag-api/pyproject.toml
COPY packages/rag-worker/pyproject.toml packages/rag-worker/pyproject.toml
COPY plugins/rag-connector-confluence/pyproject.toml plugins/rag-connector-confluence/pyproject.toml
COPY plugins/rag-connector-sharepoint/pyproject.toml plugins/rag-connector-sharepoint/pyproject.toml
COPY plugins/rag-connector-git/pyproject.toml plugins/rag-connector-git/pyproject.toml
COPY plugins/rag-connector-slack/pyproject.toml plugins/rag-connector-slack/pyproject.toml
RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --frozen --no-install-workspace --no-dev

# Project layer.
COPY . .
RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --frozen --no-dev

FROM python:3.12-slim-bookworm

# git: required by the git connector. curl: container healthchecks.
# libgl1/libglib2.0-0/libxcb1: shared libraries required by opencv (cv2),
# which docling's table-structure model imports at runtime.
RUN apt-get update \
    && apt-get install -y --no-install-recommends git curl libgl1 libglib2.0-0 libxcb1 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 1000 rag

WORKDIR /app
COPY --from=builder --chown=rag:rag /app /app
# Default blob/cache mount point; named volumes inherit this ownership.
RUN mkdir -p /data/blobs /data/cache && chown -R rag:rag /data
ENV PATH="/app/.venv/bin:$PATH" PYTHONUNBUFFERED=1
USER rag

EXPOSE 8000
CMD ["uvicorn", "rag_api.main:app", "--host", "0.0.0.0", "--port", "8000"]
