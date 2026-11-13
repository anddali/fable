# syntax=docker/dockerfile:1.7
#
# The Fable image: one `fable` binary serving the REST API, /mcp and the portal
# and running the workers (spec §2 D-1..D-5, §16 N-1/N-8).
#
#   docker build -t fable .
#   docker buildx build --platform linux/amd64,linux/arm64 -t fable .
#
# The Rust stage always runs on the build host's architecture and
# cross-compiles for TARGETARCH, so arm64 images build without emulating rustc.
# Only the tiny runtime `apt-get` step runs under QEMU for a foreign platform.
#
# The legacy Python image lives in legacy/Dockerfile.python until FAB-139.

ARG RUST_VERSION=1.100
ARG DEBIAN_RELEASE=bookworm

# ---------------------------------------------------------------------------
# UI stage (hook for FAB-123). It must leave the built portal in /ui/dist; the
# Rust stage copies it to ui/dist before `cargo build` so rust-embed can embed
# it. Until the portal exists this produces an empty directory. FAB-123
# replaces it with a Node stage on $BUILDPLATFORM, roughly:
#   FROM --platform=$BUILDPLATFORM node:22-bookworm-slim AS ui
#   WORKDIR /ui
#   COPY ui/package.json ui/package-lock.json ./
#   RUN npm ci
#   COPY ui/ ./
#   RUN npm run build
# ---------------------------------------------------------------------------
FROM --platform=$BUILDPLATFORM debian:${DEBIAN_RELEASE}-slim AS ui
RUN mkdir -p /ui/dist

# ---------------------------------------------------------------------------
# libpdfium: prebuilt shared library from bblanchon/pdfium-binaries, pinned by
# Chromium build number and verified by SHA-256 per architecture. When bumping
# PDFIUM_VERSION, update both checksums (and keep the pdfium-render API version
# in fable-parsers compatible).
# ---------------------------------------------------------------------------
FROM --platform=$BUILDPLATFORM debian:${DEBIAN_RELEASE}-slim AS pdfium
ARG TARGETARCH
ARG PDFIUM_VERSION=8152
ARG PDFIUM_SHA256_AMD64=384ab5c51882408a143a21f9c14dcd2caf1f75280ae41c554d0489b46c3dba9f
ARG PDFIUM_SHA256_ARM64=c6ed1a91234aede0cfafdc09c401b83bd44ac69deafc4599fda75d511c36403e
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) asset=pdfium-linux-x64.tgz;   sha="$PDFIUM_SHA256_AMD64" ;; \
      arm64) asset=pdfium-linux-arm64.tgz; sha="$PDFIUM_SHA256_ARM64" ;; \
      *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    curl -fsSL --retry 3 -o /tmp/pdfium.tgz \
      "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/${PDFIUM_VERSION}/${asset}"; \
    echo "${sha}  /tmp/pdfium.tgz" | sha256sum -c -; \
    mkdir -p /pdfium; \
    tar -xzf /tmp/pdfium.tgz -C /pdfium lib/libpdfium.so LICENSE licenses; \
    rm /tmp/pdfium.tgz

# ---------------------------------------------------------------------------
# Rust builder: cross-compiles the release binary for TARGETARCH.
# ---------------------------------------------------------------------------
FROM --platform=$BUILDPLATFORM rust:${RUST_VERSION}-slim-${DEBIAN_RELEASE} AS builder
ARG TARGETARCH
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) target=x86_64-unknown-linux-gnu;  gcc=gcc-x86-64-linux-gnu; libc=libc6-dev-amd64-cross ;; \
      arm64) target=aarch64-unknown-linux-gnu; gcc=gcc-aarch64-linux-gnu; libc=libc6-dev-arm64-cross ;; \
      *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    pkgs="gcc libc6-dev"; \
    if [ "$(uname -m)" != "${target%%-*}" ]; then pkgs="$pkgs $gcc $libc"; fi; \
    apt-get update; \
    apt-get install -y --no-install-recommends $pkgs; \
    rm -rf /var/lib/apt/lists/*; \
    rustup target add "$target"; \
    echo "$target" > /rust-target

# Cross linkers / C compilers (ring builds C code). Unused for native builds.
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc \
    CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc \
    SQLX_OFFLINE=true \
    CARGO_TERM_COLOR=never

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# sqlx offline query metadata (`cargo sqlx prepare --workspace`).
COPY .sqlx ./.sqlx
COPY --from=ui /ui/dist ./ui/dist

RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=fable-target-${TARGETARCH},target=/src/target,sharing=locked \
    set -eu; \
    target="$(cat /rust-target)"; \
    if [ "$(uname -m)" = "${target%%-*}" ]; then \
      unset CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER CC_x86_64_unknown_linux_gnu \
            CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER CC_aarch64_unknown_linux_gnu; \
    fi; \
    cargo build --release --locked --target "$target" -p fable; \
    install -D -m 0755 "target/$target/release/fable" /out/fable

# ---------------------------------------------------------------------------
# Runtime: debian slim + git (git connector, FAB-143) + ca-certificates +
# libpdfium. Non-root uid 1000, no volumes: the container is stateless and
# scratch space (RAG_SCRATCH_DIR, default /tmp/fable) is ephemeral (D-8).
# ---------------------------------------------------------------------------
FROM debian:${DEBIAN_RELEASE}-slim AS runtime
# git pulls in Perl (~45 MB) only for porcelain scripts (send-email, svn,
# archimport, ...). The connector uses clone/fetch/ls-tree/cat-file, which are
# C builtins, so Perl is purged to keep the image < 150 MB (D-4/N-1). The
# image never runs apt again, so the forced dependency break is harmless.
# perl-base is Essential and stays.
ARG PERL_VERSION=5.36
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git \
    && dpkg --purge --force-depends \
         git-man perl perl-modules-${PERL_VERSION} libperl${PERL_VERSION} \
         liberror-perl libgdbm-compat4 libgdbm6 \
    && rm -rf /var/lib/apt/lists/* /var/cache/debconf/*-old /var/log/* \
    && git --version \
    && groupadd --gid 1000 fable \
    && useradd --uid 1000 --gid 1000 --create-home --shell /usr/sbin/nologin fable

COPY --from=pdfium /pdfium/lib/libpdfium.so /usr/local/lib/libpdfium.so
COPY --from=pdfium /pdfium/LICENSE /usr/share/doc/libpdfium/LICENSE
COPY --from=pdfium /pdfium/licenses /usr/share/doc/libpdfium/licenses
RUN ldconfig
COPY --from=builder /out/fable /usr/local/bin/fable

USER 1000:1000
WORKDIR /home/fable
EXPOSE 8000
# D-5: no curl in the image; the binary probes its own /healthz.
HEALTHCHECK --interval=10s --timeout=5s --start-period=30s --retries=5 \
    CMD ["fable", "healthcheck"]
# D-1: with no arguments, run all roles (API + workers + migrations on boot).
# Subcommands: `docker run fable migrate`, `... --role api`, etc.
ENTRYPOINT ["fable"]
