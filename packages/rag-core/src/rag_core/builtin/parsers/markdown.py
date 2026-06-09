from __future__ import annotations

import re
from typing import ClassVar

from rag_core.interfaces import ParserPlugin
from rag_core.schemas import Block, ParsedDocument, RawDocument

_HEADING = re.compile(r"^(#{1,6})\s+(.*)$")
_FENCE = re.compile(r"^(```|~~~)")
_LIST_ITEM = re.compile(r"^\s*(?:[-*+]|\d+[.)])\s+(.*)$")


class MarkdownParser(ParserPlugin):
    """Structure-aware Markdown parser (headings, fenced code, lists, paragraphs).

    Dependency-free on purpose; swap in a CommonMark-based parser plugin if you
    need tables/footnotes fidelity.
    """

    name: ClassVar[str] = "markdown"
    media_types: ClassVar[tuple[str, ...]] = ("text/markdown", "text/x-markdown")

    def parse(self, raw: RawDocument) -> ParsedDocument:
        lines = self.decode(raw).splitlines()
        blocks: list[Block] = []
        para: list[str] = []
        code: list[str] | None = None
        title = raw.title

        def flush_para() -> None:
            nonlocal para
            text = "\n".join(para).strip()
            if text:
                blocks.append(Block(type="paragraph", text=text))
            para = []

        for line in lines:
            if code is not None:
                if _FENCE.match(line):
                    blocks.append(Block(type="code", text="\n".join(code)))
                    code = None
                else:
                    code.append(line)
                continue
            if _FENCE.match(line):
                flush_para()
                code = []
                continue
            if m := _HEADING.match(line):
                flush_para()
                level, text = len(m.group(1)), m.group(2).strip()
                blocks.append(Block(type="heading", text=text, level=level))
                if not title and level == 1:
                    title = text
                continue
            if m := _LIST_ITEM.match(line):
                flush_para()
                blocks.append(Block(type="list_item", text=m.group(1).strip()))
                continue
            if not line.strip():
                flush_para()
            else:
                para.append(line)

        if code is not None:  # unterminated fence
            blocks.append(Block(type="code", text="\n".join(code)))
        flush_para()
        return ParsedDocument(title=title, blocks=blocks, metadata=dict(raw.metadata))
