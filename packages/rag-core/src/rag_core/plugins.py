"""Entry-point based plugin discovery and instantiation."""

from __future__ import annotations

from functools import cache
from importlib.metadata import entry_points
from typing import Any

from rag_core.interfaces import (
    BasePlugin,
    ChunkerPlugin,
    ConnectorPlugin,
    EmbedderPlugin,
    ParserPlugin,
    TransformerPlugin,
)

GROUPS: dict[str, tuple[str, type[BasePlugin]]] = {
    "connector": ("rag.connectors", ConnectorPlugin),
    "parser": ("rag.parsers", ParserPlugin),
    "transformer": ("rag.transformers", TransformerPlugin),
    "chunker": ("rag.chunkers", ChunkerPlugin),
    "embedder": ("rag.embedders", EmbedderPlugin),
}


class PluginError(Exception):
    pass


class PluginNotFoundError(PluginError):
    def __init__(self, kind: str, name: str, available: list[str]):
        super().__init__(f"No {kind} plugin named {name!r}. Installed: {sorted(available)}")


@cache
def _load_group(kind: str) -> dict[str, type[BasePlugin]]:
    group, base = GROUPS[kind]
    loaded: dict[str, type[BasePlugin]] = {}
    for ep in entry_points(group=group):
        cls = ep.load()
        if not (isinstance(cls, type) and issubclass(cls, base)):
            raise PluginError(
                f"Entry point {ep.name!r} in group {group!r} does not subclass {base.__name__}"
            )
        loaded[ep.name] = cls
    return loaded


def available(kind: str) -> dict[str, type[BasePlugin]]:
    """All installed plugin classes of a kind, keyed by entry-point name."""
    if kind not in GROUPS:
        raise PluginError(f"Unknown plugin kind {kind!r}; expected one of {sorted(GROUPS)}")
    return dict(_load_group(kind))


def get_class(kind: str, name: str) -> type[BasePlugin]:
    plugins = available(kind)
    if name not in plugins:
        raise PluginNotFoundError(kind, name, list(plugins))
    return plugins[name]


def create(kind: str, name: str, config: dict[str, Any] | None = None) -> Any:
    """Instantiate a plugin, validating ``config`` against its config model."""
    return get_class(kind, name).from_config(config or {})


def select_parser(mime_type: str) -> ParserPlugin:
    """Auto-select a parser by mime type (used when the pipeline doesn't pin one)."""
    base_mime = mime_type.split(";")[0].strip().lower()
    for cls in available("parser").values():
        if base_mime in cls.media_types:  # type: ignore[attr-defined]
            return cls.from_config({})
    # text/* falls back to the plain text parser rather than failing.
    if base_mime.startswith("text/"):
        return create("parser", "text")
    raise PluginNotFoundError("parser", base_mime, list(available("parser")))


def describe_all() -> dict[str, list[dict[str, Any]]]:
    """Introspection payload for the admin API."""
    out: dict[str, list[dict[str, Any]]] = {}
    for kind in GROUPS:
        out[kind] = [
            {
                "name": name,
                "class": f"{cls.__module__}.{cls.__qualname__}",
                "config_schema": cls.config_model.model_json_schema(),
            }
            for name, cls in sorted(available(kind).items())
        ]
    return out
