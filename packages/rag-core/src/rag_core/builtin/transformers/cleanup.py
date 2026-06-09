from __future__ import annotations

import re
from typing import ClassVar

from pydantic import BaseModel, Field

from rag_core.interfaces import TransformerPlugin
from rag_core.schemas import Block, ParsedDocument

_CONTROL_CHARS = re.compile(r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]")


class CleanupConfig(BaseModel):
    # Blocks whose text matches any pattern are dropped (boilerplate, footers,
    # confidentiality notices...).
    drop_patterns: list[str] = Field(default_factory=list)
    # Drop non-heading blocks shorter than this many characters.
    min_block_chars: int = 3
    collapse_whitespace: bool = True
    # Drop a block if the identical text already appeared in this document
    # (repeated headers/footers from PDF page extraction).
    dedupe_repeated_blocks: bool = True


class BasicCleanupTransformer(TransformerPlugin):
    """Default hygiene pass: strip control chars, normalize whitespace, drop
    boilerplate and duplicate blocks."""

    name: ClassVar[str] = "basic-cleanup"
    config_model: ClassVar[type[BaseModel]] = CleanupConfig

    def transform(self, doc: ParsedDocument) -> ParsedDocument:
        cfg: CleanupConfig = self.config  # type: ignore[assignment]
        drop_res = [re.compile(p, re.IGNORECASE) for p in cfg.drop_patterns]
        seen: set[str] = set()
        out: list[Block] = []
        for block in doc.blocks:
            text = _CONTROL_CHARS.sub("", block.text)
            if cfg.collapse_whitespace and block.type != "code":
                text = re.sub(r"[ \t]+", " ", text)
                text = re.sub(r"\n{3,}", "\n\n", text).strip()
            if not text:
                continue
            if block.type != "heading" and len(text) < cfg.min_block_chars:
                continue
            if any(p.search(text) for p in drop_res):
                continue
            if cfg.dedupe_repeated_blocks and block.type != "heading":
                key = text.casefold()
                if key in seen:
                    continue
                seen.add(key)
            out.append(block.model_copy(update={"text": text}))
        return doc.model_copy(update={"blocks": out})
