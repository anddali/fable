from __future__ import annotations

from html.parser import HTMLParser as _StdHTMLParser
from typing import ClassVar

from rag_core.interfaces import ParserPlugin
from rag_core.schemas import Block, ParsedDocument, RawDocument

_HEADINGS = {"h1": 1, "h2": 2, "h3": 3, "h4": 4, "h5": 5, "h6": 6}
_SKIP = {"script", "style", "head", "noscript"}
_BLOCK_BREAKERS = {"p", "div", "section", "article", "br", "tr", "table"}


class _Extractor(_StdHTMLParser):
    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.blocks: list[Block] = []
        self._buf: list[str] = []
        self._mode: tuple[str, int] = ("paragraph", 0)  # (block type, heading level)
        self._skip_depth = 0

    def _flush(self) -> None:
        text = " ".join("".join(self._buf).split())
        self._buf = []
        if text:
            btype, level = self._mode
            self.blocks.append(Block(type=btype, text=text, level=level))  # type: ignore[arg-type]
        self._mode = ("paragraph", 0)

    def handle_starttag(self, tag: str, attrs) -> None:
        if tag in _SKIP:
            self._skip_depth += 1
            return
        if tag in _HEADINGS:
            self._flush()
            self._mode = ("heading", _HEADINGS[tag])
        elif tag == "li":
            self._flush()
            self._mode = ("list_item", 0)
        elif tag in ("pre", "code") and not self._buf:
            self._flush()
            self._mode = ("code", 0)
        elif tag in _BLOCK_BREAKERS:
            self._flush()

    def handle_endtag(self, tag: str) -> None:
        if tag in _SKIP:
            self._skip_depth = max(0, self._skip_depth - 1)
            return
        if tag in _HEADINGS or tag in ("li", "pre") or tag in _BLOCK_BREAKERS:
            self._flush()

    def handle_data(self, data: str) -> None:
        if not self._skip_depth:
            self._buf.append(data)


class HtmlParser(ParserPlugin):
    """HTML / Confluence storage-format (XHTML) to structured blocks."""

    name: ClassVar[str] = "html"
    media_types: ClassVar[tuple[str, ...]] = ("text/html", "application/xhtml+xml")

    def parse(self, raw: RawDocument) -> ParsedDocument:
        extractor = _Extractor()
        extractor.feed(self.decode(raw))
        extractor._flush()
        return ParsedDocument(title=raw.title, blocks=extractor.blocks, metadata=dict(raw.metadata))
