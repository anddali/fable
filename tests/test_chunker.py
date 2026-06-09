from rag_core.builtin.chunkers.default import DefaultChunker, estimate_tokens
from rag_core.schemas import Block, ParsedDocument


def make_chunker(**config) -> DefaultChunker:
    return DefaultChunker.from_config(config)


def doc(*blocks: Block) -> ParsedDocument:
    return ParsedDocument(title="t", blocks=list(blocks))


def test_chunks_have_no_overlap():
    """Every source character appears in exactly one chunk."""
    blocks = [Block(text=f"Paragraph number {i} with some words in it.") for i in range(50)]
    chunks = make_chunker(max_tokens=64, include_heading_path=False).chunk(doc(*blocks))

    reassembled = "\n\n".join(c.text for c in chunks)
    for block in blocks:
        assert reassembled.count(block.text) == 1


def test_respects_max_tokens():
    blocks = [Block(text="word " * 100) for _ in range(10)]
    chunks = make_chunker(max_tokens=200).chunk(doc(*blocks))
    assert all(c.token_count <= 220 for c in chunks)  # small slack for joins
    assert len(chunks) > 1


def test_heading_path_breadcrumb():
    chunks = make_chunker().chunk(
        doc(
            Block(type="heading", text="Guide", level=1),
            Block(type="heading", text="Setup", level=2),
            Block(text="Install the thing."),
            Block(type="heading", text="Usage", level=2),
            Block(text="Use the thing."),
        )
    )
    assert chunks[0].metadata["heading_path"] == "Guide > Setup"
    assert chunks[1].metadata["heading_path"] == "Guide > Usage"
    assert chunks[0].text.startswith("[Guide > Setup]")


def test_heading_starts_new_chunk():
    chunks = make_chunker(include_heading_path=False).chunk(
        doc(
            Block(type="heading", text="A", level=1),
            Block(text="alpha"),
            Block(type="heading", text="B", level=1),
            Block(text="beta"),
        )
    )
    assert [c.text for c in chunks] == ["alpha", "beta"]
    assert [c.ordinal for c in chunks] == [0, 1]


def test_oversized_block_is_sentence_split_without_overlap():
    text = " ".join(f"This is sentence number {i}." for i in range(200))
    chunks = make_chunker(max_tokens=64, include_heading_path=False).chunk(doc(Block(text=text)))
    assert len(chunks) > 1
    for i in range(200):
        marker = f"sentence number {i}."
        assert sum(c.text.count(marker) for c in chunks) == 1


def test_empty_document():
    assert make_chunker().chunk(ParsedDocument()) == []


def test_estimate_tokens_floor():
    assert estimate_tokens("ab") == 1
