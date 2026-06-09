from __future__ import annotations

import re
from typing import ClassVar

from rag_core.interfaces import ParserPlugin
from rag_core.schemas import Block, ParsedDocument, RawDocument


class TextParser(ParserPlugin):
    """Plain text: paragraphs separated by blank lines."""

    name: ClassVar[str] = "text"
    media_types: ClassVar[tuple[str, ...]] = ("text/plain",)

    def parse(self, raw: RawDocument) -> ParsedDocument:
        text = self.decode(raw)
        blocks = [
            Block(type="paragraph", text=para.strip())
            for para in re.split(r"\n\s*\n", text)
            if para.strip()
        ]
        return ParsedDocument(title=raw.title, blocks=blocks, metadata=dict(raw.metadata))
