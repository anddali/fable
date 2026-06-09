from __future__ import annotations

import io
import re
from typing import ClassVar

from pypdf import PdfReader

from rag_core.interfaces import ParserPlugin
from rag_core.schemas import Block, ParsedDocument, RawDocument


class PdfParser(ParserPlugin):
    """Text-layer PDF extraction via pypdf, one set of paragraph blocks per page.

    Scanned/image-only PDFs yield no text; pair with an OCR transformer plugin
    if that matters for your corpus.
    """

    name: ClassVar[str] = "pdf"
    media_types: ClassVar[tuple[str, ...]] = ("application/pdf",)

    def parse(self, raw: RawDocument) -> ParsedDocument:
        if raw.content is None:
            raise ValueError("PDF parser requires binary content")
        reader = PdfReader(io.BytesIO(raw.content))
        blocks: list[Block] = []
        for page_no, page in enumerate(reader.pages, start=1):
            text = page.extract_text() or ""
            for para in re.split(r"\n\s*\n", text):
                cleaned = para.strip()
                if cleaned:
                    blocks.append(Block(type="paragraph", text=cleaned, metadata={"page": page_no}))
        title = raw.title or ((reader.metadata.title or "") if reader.metadata else "")
        return ParsedDocument(title=title, blocks=blocks, metadata=dict(raw.metadata))
