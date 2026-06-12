"""Layout-aware document parsing via Docling (PDF, DOCX, PPTX).

Docling runs layout-analysis and table-structure models locally, producing a
structured document with headings, reading order, tables and figures. Figures
are emitted as ``image`` blocks (PNG bytes, base64 in metadata) for the
pipeline's optional vision-LLM description step.

The ``docling`` package (and its torch stack) is an optional extra installed
only where parsing happens (the worker); it is imported lazily so that plugin
discovery in the API process stays lightweight.
"""

from __future__ import annotations

import base64
import io
from typing import Any, ClassVar, Literal

from pydantic import BaseModel, Field

from rag_core.interfaces import ParserPlugin
from rag_core.schemas import Block, ParsedDocument, RawDocument

_EXTENSION_BY_MIME = {
    "application/pdf": ".pdf",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document": ".docx",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation": ".pptx",
}

# Converters hold loaded ML models; keyed by config so repeated jobs in the
# long-lived worker process reuse them instead of reloading per document.
_converters: dict[tuple, Any] = {}


class DoclingParserConfig(BaseModel):
    # OCR is off by default: it pulls in large extra models and most corpora
    # have text layers. Scanned documents need ocr=True to yield anything.
    ocr: bool = False
    table_mode: Literal["fast", "accurate"] = "accurate"
    # Emit figure crops as image blocks for the vision_llm pipeline step.
    extract_images: bool = True
    images_scale: float = Field(default=2.0, ge=0.5, le=4.0)


def _page_of(item: Any) -> dict[str, Any]:
    prov = getattr(item, "prov", None) or []
    if prov:
        page_no = getattr(prov[0], "page_no", None)
        if page_no is not None:
            return {"page": page_no}
    return {}


def _table_blocks(item: Any, meta: dict[str, Any]) -> list[Block]:
    """One ``table_row`` block per data row, ``header: value`` pairs joined by
    `` | `` — mirrors the tabular parser's convention so chunks read naturally.
    """
    data = getattr(item, "data", None)
    grid = getattr(data, "grid", None) or []
    rows = [[(getattr(cell, "text", "") or "").strip() for cell in row] for row in grid]
    rows = [row for row in rows if any(row)]
    if not rows:
        return []
    header, *body = rows
    if not body:
        # Header-only (or single-row) table: index the row as-is.
        return [Block(type="table_row", text=" | ".join(filter(None, header)), metadata=dict(meta))]
    blocks = []
    for row in body:
        pairs = [f"{h}: {v}" if h else v for h, v in zip(header, row) if v]
        if pairs:
            blocks.append(Block(type="table_row", text=" | ".join(pairs), metadata=dict(meta)))
    return blocks


def _image_block(item: Any, doc: Any, meta: dict[str, Any]) -> Block | None:
    image = item.get_image(doc)
    if image is None:
        return None
    buf = io.BytesIO()
    image.save(buf, format="PNG")
    return Block(
        type="image",
        text="",
        metadata={
            **meta,
            "image_b64": base64.b64encode(buf.getvalue()).decode("ascii"),
            "image_mime": "image/png",
        },
    )


def blocks_from_docling(doc: Any, *, extract_images: bool = True) -> list[Block]:
    """Map a DoclingDocument's reading-order items onto platform Block types.

    Duck-typed on item ``label``/``text``/``prov`` so it is testable without
    the docling models installed.
    """
    blocks: list[Block] = []
    for item, _level in doc.iterate_items():
        label = str(getattr(item, "label", "") or "")
        meta = _page_of(item)
        if label == "table":
            blocks.extend(_table_blocks(item, meta))
            continue
        if label == "picture":
            if extract_images:
                block = _image_block(item, doc, meta)
                if block is not None:
                    blocks.append(block)
            continue
        text = (getattr(item, "text", "") or "").strip()
        if not text:
            continue
        if label == "title":
            blocks.append(Block(type="heading", text=text, level=1, metadata=meta))
        elif label == "section_header":
            level = getattr(item, "level", 1) or 1
            # Document title occupies level 1; sections start below it.
            blocks.append(Block(type="heading", text=text, level=level + 1, metadata=meta))
        elif label == "code":
            blocks.append(Block(type="code", text=text, metadata=meta))
        elif label == "list_item":
            blocks.append(Block(type="list_item", text=text, metadata=meta))
        else:
            # text, paragraph, caption, footnote, formula, checkbox, ...
            blocks.append(Block(type="paragraph", text=text, metadata=meta))
    return blocks


class DoclingParser(ParserPlugin):
    """Docling layout-aware parser for PDF/DOCX/PPTX.

    Wins auto-selection over the plain pypdf parser; pin
    ``{"parser": {"name": "pdf"}}`` on a collection to opt out.
    """

    name: ClassVar[str] = "docling"
    config_model: ClassVar[type[BaseModel]] = DoclingParserConfig
    priority: ClassVar[int] = 10
    media_types: ClassVar[tuple[str, ...]] = tuple(_EXTENSION_BY_MIME)

    def parse(self, raw: RawDocument) -> ParsedDocument:
        if raw.content is None:
            raise ValueError("Docling parser requires binary content")
        cfg: DoclingParserConfig = self.config  # type: ignore[assignment]
        converter = _get_converter(cfg)

        from docling.datamodel.base_models import DocumentStream

        extension = _EXTENSION_BY_MIME.get(raw.mime_type.split(";")[0].strip().lower(), ".pdf")
        stream = DocumentStream(name=f"document{extension}", stream=io.BytesIO(raw.content))
        result = converter.convert(stream)
        doc = result.document

        blocks = blocks_from_docling(doc, extract_images=cfg.extract_images)
        title = raw.title or (getattr(doc, "name", "") or "")
        return ParsedDocument(title=title, blocks=blocks, metadata=dict(raw.metadata))


def _get_converter(cfg: DoclingParserConfig) -> Any:
    key = (cfg.ocr, cfg.table_mode, cfg.extract_images, cfg.images_scale)
    if key in _converters:
        return _converters[key]
    try:
        from docling.datamodel.base_models import InputFormat
        from docling.datamodel.pipeline_options import PdfPipelineOptions, TableFormerMode
        from docling.document_converter import DocumentConverter, PdfFormatOption
    except ImportError as exc:  # pragma: no cover
        raise RuntimeError(
            "The 'docling' parser requires the docling package; "
            "install rag-core with the [docling] extra"
        ) from exc

    pdf_options = PdfPipelineOptions()
    pdf_options.do_ocr = cfg.ocr
    pdf_options.do_table_structure = True
    pdf_options.table_structure_options.mode = (
        TableFormerMode.ACCURATE if cfg.table_mode == "accurate" else TableFormerMode.FAST
    )
    pdf_options.generate_picture_images = cfg.extract_images
    pdf_options.images_scale = cfg.images_scale

    converter = DocumentConverter(
        format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=pdf_options)}
    )
    _converters[key] = converter
    return converter
