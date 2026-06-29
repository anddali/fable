"""Index-inspector API: route registration, DTO shaping, MCP exclusion."""

import uuid

from rag_api.dto import ChunkOut, DocumentWithChunksOut


def test_documents_routes_registered():
    from rag_api.routers import documents

    paths = {route.path for route in documents.router.routes}
    assert paths == {
        "/collections/{collection_id}/documents",
        "/collections/{collection_id}/documents/{document_id}",
        "/collections/{collection_id}/documents/{document_id}/chunks",
    }
    # Read-only inspector: no mutating methods.
    methods = {m for route in documents.router.routes for m in route.methods}
    assert methods == {"GET"}


def test_chunk_out_never_carries_embedding():
    assert "embedding" not in ChunkOut.model_fields
    chunk = ChunkOut(
        id=uuid.uuid4(),
        ordinal=0,
        token_count=12,
        text="hello",
        chunk_metadata={"heading_path": "A > B"},
        has_embedding=True,
    )
    assert chunk.has_embedding is True
    assert "embedding" not in chunk.model_dump()


def test_document_with_chunks_defaults():
    fields = DocumentWithChunksOut.model_fields
    assert fields["chunk_count"].default == 0
    assert fields["size_bytes"].default == 0


def test_documents_not_exposed_over_mcp():
    """MCP stays search-only even with the documents router mounted."""
    from fastapi import FastAPI
    from fastapi_mcp import FastApiMCP

    from rag_api.routers import documents, search

    app = FastAPI()
    app.include_router(search.router)
    app.include_router(documents.router)
    mcp = FastApiMCP(app, include_operations=["search_documents", "list_searchable_collections"])
    assert {t.name for t in mcp.tools} == {"search_documents", "list_searchable_collections"}
