from __future__ import annotations

import csv
import io
from typing import ClassVar

import openpyxl
from pydantic import BaseModel

from rag_core.interfaces import ParserPlugin
from rag_core.schemas import Block, ParsedDocument, RawDocument

XLSX_MIME = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"


class TabularConfig(BaseModel):
    # Treat the first row of each sheet/file as the header row.
    header_row: bool = True
    max_rows: int = 50_000


class TabularParser(ParserPlugin):
    """CSV/XLSX: each data row becomes one ``table_row`` block of "col: value"
    pairs, so chunks stay row-aligned and self-describing."""

    name: ClassVar[str] = "tabular"
    media_types: ClassVar[tuple[str, ...]] = ("text/csv", XLSX_MIME)
    config_model: ClassVar[type[BaseModel]] = TabularConfig

    def parse(self, raw: RawDocument) -> ParsedDocument:
        base_mime = raw.mime_type.split(";")[0].strip().lower()
        if base_mime == XLSX_MIME:
            blocks = self._parse_xlsx(raw)
        else:
            blocks = self._rows_to_blocks(csv.reader(io.StringIO(self.decode(raw))), sheet=None)
        return ParsedDocument(title=raw.title, blocks=blocks, metadata=dict(raw.metadata))

    def _parse_xlsx(self, raw: RawDocument) -> list[Block]:
        if raw.content is None:
            raise ValueError("XLSX parser requires binary content")
        wb = openpyxl.load_workbook(io.BytesIO(raw.content), read_only=True, data_only=True)
        blocks: list[Block] = []
        for ws in wb.worksheets:
            rows = (
                ["" if cell is None else str(cell) for cell in row]
                for row in ws.iter_rows(values_only=True)
            )
            blocks.extend(self._rows_to_blocks(rows, sheet=ws.title))
        wb.close()
        return blocks

    def _rows_to_blocks(self, rows, sheet: str | None) -> list[Block]:
        cfg: TabularConfig = self.config  # type: ignore[assignment]
        blocks: list[Block] = []
        header: list[str] | None = None
        if sheet:
            blocks.append(Block(type="heading", text=f"Sheet: {sheet}", level=2))
        for i, row in enumerate(rows):
            if i >= cfg.max_rows:
                break
            cells = [str(c).strip() for c in row]
            if not any(cells):
                continue
            if cfg.header_row and header is None:
                header = cells
                continue
            if header:
                pairs = [f"{h}: {v}" for h, v in zip(header, cells, strict=False) if v]
                text = "; ".join(pairs)
            else:
                text = "; ".join(c for c in cells if c)
            if text:
                meta = {"row": i + 1} | ({"sheet": sheet} if sheet else {})
                blocks.append(Block(type="table_row", text=text, metadata=meta))
        return blocks
