from rag_core.builtin.parsers.html import HtmlParser
from rag_core.builtin.parsers.markdown import MarkdownParser
from rag_core.builtin.parsers.tabular import TabularParser
from rag_core.builtin.parsers.text import TextParser
from rag_core.schemas import RawDocument


def raw(text: str, mime: str = "text/plain", **kw) -> RawDocument:
    return RawDocument(external_id="x", text=text, mime_type=mime, **kw)


def test_text_parser_paragraphs():
    parsed = TextParser.from_config({}).parse(raw("one\n\ntwo\n\n\nthree"))
    assert [b.text for b in parsed.blocks] == ["one", "two", "three"]


def test_markdown_structure():
    md = "# Title\n\nIntro para.\n\n## Section\n\n- item one\n- item two\n\n```\ncode here\n```\n"
    parsed = MarkdownParser.from_config({}).parse(raw(md, "text/markdown"))
    types = [(b.type, b.text) for b in parsed.blocks]
    assert ("heading", "Title") in types
    assert ("heading", "Section") in types
    assert ("list_item", "item one") in types
    assert ("code", "code here") in types
    assert parsed.title == "Title"  # inferred from the h1


def test_html_parser_blocks():
    html = "<html><head><style>x{}</style></head><body><h1>Hello</h1><p>World <b>bold</b></p><ul><li>a</li><li>b</li></ul></body></html>"
    parsed = HtmlParser.from_config({}).parse(raw(html, "text/html"))
    assert parsed.blocks[0].type == "heading"
    assert parsed.blocks[0].text == "Hello"
    assert any(b.text == "World bold" for b in parsed.blocks)
    assert sum(1 for b in parsed.blocks if b.type == "list_item") == 2
    assert not any("x{}" in b.text for b in parsed.blocks)  # style stripped


def test_html_parser_emits_image_blocks_for_data_uris():
    import base64

    b64 = base64.b64encode(b"fake-png-bytes").decode("ascii")
    html = (
        f'<p>Before</p><img src="data:image/png;base64,{b64}" alt="the diagram"/>'
        f'<p>After</p><img src="https://example.com/remote.png">'
    )
    parsed = HtmlParser.from_config({}).parse(raw(html, "text/html"))
    images = [b for b in parsed.blocks if b.type == "image"]
    assert len(images) == 1  # URL-referenced images carry no bytes -> skipped
    assert images[0].metadata["image_b64"] == b64
    assert images[0].metadata["image_mime"] == "image/png"
    assert images[0].metadata["alt"] == "the diagram"
    texts = [b.text for b in parsed.blocks if b.type == "paragraph"]
    assert texts == ["Before", "After"]


def test_csv_rows_become_labeled_blocks():
    csv_text = "name,role\nada,engineer\ngrace,admiral\n"
    parsed = TabularParser.from_config({}).parse(raw(csv_text, "text/csv"))
    assert [b.text for b in parsed.blocks] == [
        "name: ada; role: engineer",
        "name: grace; role: admiral",
    ]
    assert parsed.blocks[0].metadata["row"] == 2


def test_csv_without_header():
    parsed = TabularParser.from_config({"header_row": False}).parse(raw("a,b\nc,d", "text/csv"))
    assert [b.text for b in parsed.blocks] == ["a; b", "c; d"]
