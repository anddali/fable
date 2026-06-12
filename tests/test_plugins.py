import pytest
from pydantic import ValidationError

from rag_core import plugins


def test_builtin_plugins_discovered():
    assert "upload" in plugins.available("connector")
    assert {"text", "markdown", "html", "pdf", "tabular", "docling"} <= set(
        plugins.available("parser")
    )
    assert "basic-cleanup" in plugins.available("transformer")
    assert "default" in plugins.available("chunker")
    assert {"openai", "fake"} <= set(plugins.available("embedder"))
    assert {"openai-compat", "anthropic"} <= set(plugins.available("vision"))


def test_external_connector_plugins_discovered():
    connectors = set(plugins.available("connector"))
    assert {"confluence", "sharepoint", "git", "slack"} <= connectors


def test_unknown_plugin_raises_with_candidates():
    with pytest.raises(plugins.PluginNotFoundError, match="default"):
        plugins.create("chunker", "nope")


def test_create_validates_config():
    chunker = plugins.create("chunker", "default", {"max_tokens": 128})
    assert chunker.config.max_tokens == 128
    with pytest.raises(ValidationError):
        plugins.create("chunker", "default", {"max_tokens": 1})  # below ge=32


def test_select_parser_by_mime():
    assert type(plugins.select_parser("text/markdown")).name == "markdown"
    # docling outranks the plain pypdf parser for PDFs (priority 10 vs 0).
    assert type(plugins.select_parser("application/pdf; charset=x")).name == "docling"
    # Unknown text/* subtype falls back to the plain text parser.
    assert type(plugins.select_parser("text/x-unknown")).name == "text"


def test_describe_all_includes_config_schema():
    described = plugins.describe_all()
    chunkers = {p["name"]: p for p in described["chunker"]}
    assert "max_tokens" in chunkers["default"]["config_schema"]["properties"]
