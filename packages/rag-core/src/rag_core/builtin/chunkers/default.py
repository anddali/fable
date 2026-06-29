from __future__ import annotations

import re
from typing import ClassVar

from pydantic import BaseModel, Field

from rag_core.interfaces import ChunkerPlugin
from rag_core.schemas import Block, ChunkDraft, ParsedDocument

_SENTENCE_END = re.compile(r"(?<=[.!?])\s+")


def estimate_tokens(text: str) -> int:
    """Cheap tokenizer-free estimate (~4 chars/token for English-like text)."""
    return max(1, len(text) // 4)


class DefaultChunkerConfig(BaseModel):
    max_tokens: int = Field(default=480, ge=32)
    # Prefix each chunk with its heading breadcrumb ("Guide > Setup > Auth")
    # so chunks remain self-contained for embedding and display.
    include_heading_path: bool = True


class DefaultChunker(ChunkerPlugin):
    """Structure-aware, strictly non-overlapping chunker.

    Packs consecutive blocks into chunks up to ``max_tokens``, starting a new
    chunk at every heading. Source text is never re-emitted: every character
    lands in exactly one chunk (the heading breadcrumb prefix is metadata-like
    context, not source overlap). Oversized blocks split on sentence
    boundaries, hard-splitting as a last resort.
    """

    name: ClassVar[str] = "default"
    config_model: ClassVar[type[BaseModel]] = DefaultChunkerConfig

    def chunk(self, doc: ParsedDocument) -> list[ChunkDraft]:
        cfg: DefaultChunkerConfig = self.config  # type: ignore[assignment]
        chunks: list[ChunkDraft] = []
        heading_path: list[tuple[int, str]] = []
        current: list[str] = []
        current_tokens = 0
        current_path = ""
        # Figure refs (set by the vision pipeline step) of the blocks packed
        # into the running chunk; surfaced as chunk_metadata["figures"] so
        # search hits can link back to the stored image crops.
        current_figures: list[dict] = []

        def flush() -> None:
            nonlocal current, current_tokens, current_figures
            body = "\n\n".join(current).strip()
            figures = current_figures
            current, current_tokens, current_figures = [], 0, []
            if not body:
                return
            text = (
                f"[{current_path}]\n{body}" if cfg.include_heading_path and current_path else body
            )
            metadata: dict = {"heading_path": current_path} if current_path else {}
            if figures:
                metadata["figures"] = figures
            chunks.append(
                ChunkDraft(
                    ordinal=len(chunks),
                    text=text,
                    token_count=estimate_tokens(text),
                    metadata=metadata,
                )
            )

        for block in doc.blocks:
            if block.type == "heading":
                # Section boundary: close the running chunk, update breadcrumb.
                flush()
                while heading_path and heading_path[-1][0] >= block.level:
                    heading_path.pop()
                heading_path.append((block.level, block.text))
                current_path = " > ".join(text for _, text in heading_path)
                continue
            figure = block.metadata.get("figure")
            for piece in self._split_oversized(block, cfg.max_tokens):
                tokens = estimate_tokens(piece)
                if current and current_tokens + tokens > cfg.max_tokens:
                    flush()
                current.append(piece)
                current_tokens += tokens
                if figure:
                    current_figures.append(figure)
                    figure = None  # split pieces share one source figure

        flush()
        return chunks

    def _split_oversized(self, block: Block, max_tokens: int) -> list[str]:
        char_limit = max_tokens * 4  # mirrors the estimate_tokens heuristic
        if len(block.text) <= char_limit:
            return [block.text]
        # Hard-split only single sentences that alone exceed the budget, so
        # normal sentences are never cut mid-text.
        sentences: list[str] = []
        for sentence in _SENTENCE_END.split(block.text):
            while len(sentence) > char_limit:
                sentences.append(sentence[:char_limit])
                sentence = sentence[char_limit:]
            sentences.append(sentence)
        pieces: list[str] = []
        buf = ""
        for sentence in sentences:
            candidate = f"{buf} {sentence}".strip() if buf else sentence
            if buf and len(candidate) > char_limit:
                pieces.append(buf)
                buf = sentence
            else:
                buf = candidate
        if buf:
            pieces.append(buf)
        return pieces
