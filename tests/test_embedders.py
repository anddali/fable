import math

from rag_core.builtin.embedders.fake import FakeEmbedder


async def test_fake_embedder_deterministic_and_normalized():
    embedder = FakeEmbedder.from_config({"dim": 64})
    a1, a2, b = await embedder.embed(["alpha", "alpha", "beta"])
    assert a1 == a2
    assert a1 != b
    assert len(a1) == 64
    assert math.isclose(sum(v * v for v in a1), 1.0, rel_tol=1e-6)
