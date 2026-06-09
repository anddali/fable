from rag_core.builtin.transformers.cleanup import BasicCleanupTransformer
from rag_core.schemas import Block, ParsedDocument


def transform(blocks, **config):
    transformer = BasicCleanupTransformer.from_config(config)
    return transformer.transform(ParsedDocument(blocks=blocks)).blocks


def test_whitespace_normalized_and_control_chars_stripped():
    out = transform([Block(text="hello \t  world\x00\x07 again")])
    assert out[0].text == "hello world again"


def test_drop_patterns():
    blocks = [
        Block(text="Real content about things."),
        Block(text="CONFIDENTIAL - do not distribute"),
    ]
    out = transform(blocks, drop_patterns=["confidential"])
    assert [b.text for b in out] == ["Real content about things."]


def test_repeated_boilerplate_deduped():
    footer = Block(text="Page footer — Acme Corp 2026")
    out = transform([Block(text="content one"), footer, Block(text="content two"), footer])
    assert sum(1 for b in out if "footer" in b.text) == 1


def test_short_blocks_dropped_but_headings_kept():
    out = transform([Block(text="ab"), Block(type="heading", text="A", level=1)])
    assert len(out) == 1
    assert out[0].type == "heading"


def test_code_blocks_keep_whitespace():
    out = transform([Block(type="code", text="def f():\n    return  1")])
    assert out[0].text == "def f():\n    return  1"
