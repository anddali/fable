//! Lookups over everything registered with `inventory` (spec §8 P-3, P-7,
//! P-8).

use serde::Serialize;
use serde_json::Value;

use crate::plugin::{
    BuildContext, Chunker, ChunkerRegistration, ConnectionKindRegistration, Connector,
    ConnectorRegistration, Embedder, EmbedderRegistration, Parser, ParserRegistration, PluginError,
    PluginKind, PluginMeta, Transformer, TransformerRegistration, Vision, VisionRegistration,
};

/// Implemented by every registration type so lookups can be generic.
pub trait Registered: inventory::Collect {
    /// The plugin kind.
    const KIND: PluginKind;
    /// Common metadata.
    fn meta(&self) -> &PluginMeta;
}

macro_rules! registered {
    ($reg:ty, $kind:expr) => {
        impl Registered for $reg {
            const KIND: PluginKind = $kind;
            fn meta(&self) -> &PluginMeta {
                &self.meta
            }
        }
    };
}

registered!(ConnectorRegistration, PluginKind::Connector);
registered!(ParserRegistration, PluginKind::Parser);
registered!(TransformerRegistration, PluginKind::Transformer);
registered!(ChunkerRegistration, PluginKind::Chunker);
registered!(EmbedderRegistration, PluginKind::Embedder);
registered!(VisionRegistration, PluginKind::Vision);

/// All registrations of a kind, sorted by name.
pub fn all<R: Registered>() -> Vec<&'static R> {
    let mut v: Vec<&'static R> = inventory::iter::<R>.into_iter().collect();
    v.sort_by_key(|r| r.meta().name);
    v
}

/// The registration of a kind with this name.
pub fn get<R: Registered>(name: &str) -> Result<&'static R, PluginError> {
    inventory::iter::<R>
        .into_iter()
        .find(|r| r.meta().name == name)
        .ok_or_else(|| PluginError::NotFound {
            kind: R::KIND,
            name: name.to_owned(),
        })
}

/// Validate a plugin config at onboarding time (P-2): unknown plugin →
/// `NotFound`, bad config → `InvalidConfig` with field errors.
pub fn validate<R: Registered>(name: &str, config: &Value) -> Result<(), PluginError> {
    get::<R>(name)?.meta().validate(config)?;
    Ok(())
}

/// Build a connector by name.
pub fn connector(
    name: &str,
    config: &Value,
    ctx: BuildContext,
) -> Result<Box<dyn Connector>, PluginError> {
    (get::<ConnectorRegistration>(name)?.build)(config, ctx)
}

/// Build a parser by name.
pub fn parser(
    name: &str,
    config: &Value,
    ctx: BuildContext,
) -> Result<Box<dyn Parser>, PluginError> {
    (get::<ParserRegistration>(name)?.build)(config, ctx)
}

/// Build a transformer by name.
pub fn transformer(
    name: &str,
    config: &Value,
    ctx: BuildContext,
) -> Result<Box<dyn Transformer>, PluginError> {
    (get::<TransformerRegistration>(name)?.build)(config, ctx)
}

/// Build a chunker by name.
pub fn chunker(
    name: &str,
    config: &Value,
    ctx: BuildContext,
) -> Result<Box<dyn Chunker>, PluginError> {
    (get::<ChunkerRegistration>(name)?.build)(config, ctx)
}

/// Build an embedder by name.
pub fn embedder(
    name: &str,
    config: &Value,
    ctx: BuildContext,
) -> Result<Box<dyn Embedder>, PluginError> {
    (get::<EmbedderRegistration>(name)?.build)(config, ctx)
}

/// Build a vision plugin by name.
pub fn vision(
    name: &str,
    config: &Value,
    ctx: BuildContext,
) -> Result<Box<dyn Vision>, PluginError> {
    (get::<VisionRegistration>(name)?.build)(config, ctx)
}

/// Auto-select a parser by MIME type (P-7): the highest-priority parser
/// claiming the base type (parameters and case ignored, ties broken by
/// name); `text/*` falls back to the `text` parser. An explicit
/// `pipeline.parser` bypasses this.
pub fn select_parser(mime_type: &str) -> Result<&'static ParserRegistration, PluginError> {
    let base = mime_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let best = all::<ParserRegistration>()
        .into_iter()
        .filter(|p| p.media_types.iter().any(|m| m.eq_ignore_ascii_case(&base)))
        .max_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| b.meta.name.cmp(a.meta.name))
        });
    if let Some(p) = best {
        return Ok(p);
    }
    if base.starts_with("text/") {
        if let Ok(p) = get::<ParserRegistration>("text") {
            return Ok(p);
        }
    }
    Err(PluginError::NoParser(base))
}

/// All registered connection kinds, sorted by kind.
pub fn connection_kinds() -> Vec<&'static ConnectionKindRegistration> {
    let mut v: Vec<_> = inventory::iter::<ConnectionKindRegistration>
        .into_iter()
        .collect();
    v.sort_by_key(|k| k.kind);
    v
}

/// A connection kind by name.
pub fn connection_kind(kind: &str) -> Option<&'static ConnectionKindRegistration> {
    inventory::iter::<ConnectionKindRegistration>
        .into_iter()
        .find(|k| k.kind == kind)
}

// ---------------------------------------------------------------------------
// Catalog (GET /plugins payload, P-8)
// ---------------------------------------------------------------------------

/// Everything installed, with JSON Schemas, for `GET /plugins`.
#[derive(Debug, Clone, Serialize)]
pub struct PluginCatalog {
    /// Connectors.
    pub connector: Vec<ConnectorInfo>,
    /// Parsers.
    pub parser: Vec<ParserInfo>,
    /// Transformers.
    pub transformer: Vec<PluginInfo>,
    /// Chunkers.
    pub chunker: Vec<PluginInfo>,
    /// Embedders.
    pub embedder: Vec<PluginInfo>,
    /// Vision plugins.
    pub vision: Vec<PluginInfo>,
    /// Connection kinds with config and secret schemas.
    pub connection_kinds: Vec<ConnectionKindInfo>,
}

/// Common catalog entry.
#[derive(Debug, Clone, Serialize)]
pub struct PluginInfo {
    /// Stable name.
    pub name: &'static str,
    /// Description.
    pub description: &'static str,
    /// Accepted connection kinds (empty = none needed).
    pub connection_kinds: &'static [&'static str],
    /// JSON Schema of the config.
    pub config_schema: Value,
}

impl From<&PluginMeta> for PluginInfo {
    fn from(m: &PluginMeta) -> Self {
        Self {
            name: m.name,
            description: m.description,
            connection_kinds: m.connection_kinds,
            config_schema: m.schema(),
        }
    }
}

/// Connector catalog entry.
#[derive(Debug, Clone, Serialize)]
pub struct ConnectorInfo {
    /// Common fields.
    #[serde(flatten)]
    pub info: PluginInfo,
    /// Incremental listing.
    pub incremental: bool,
    /// Accepts pasted URLs as targets.
    pub supports_url_targets: bool,
}

/// Parser catalog entry.
#[derive(Debug, Clone, Serialize)]
pub struct ParserInfo {
    /// Common fields.
    #[serde(flatten)]
    pub info: PluginInfo,
    /// MIME types handled.
    pub media_types: &'static [&'static str],
    /// Auto-selection priority.
    pub priority: i32,
}

/// Connection-kind catalog entry.
#[derive(Debug, Clone, Serialize)]
pub struct ConnectionKindInfo {
    /// Kind identifier.
    pub kind: &'static str,
    /// Label.
    pub label: &'static str,
    /// JSON Schema of the non-secret config.
    pub config_schema: Value,
    /// JSON Schema of the secret.
    pub secret_schema: Value,
}

fn infos<R: Registered>() -> Vec<PluginInfo> {
    all::<R>().into_iter().map(|r| r.meta().into()).collect()
}

/// Build the catalog.
pub fn catalog() -> PluginCatalog {
    PluginCatalog {
        connector: all::<ConnectorRegistration>()
            .into_iter()
            .map(|r| ConnectorInfo {
                info: (&r.meta).into(),
                incremental: r.incremental,
                supports_url_targets: r.supports_url_targets,
            })
            .collect(),
        parser: all::<ParserRegistration>()
            .into_iter()
            .map(|r| ParserInfo {
                info: (&r.meta).into(),
                media_types: r.media_types,
                priority: r.priority,
            })
            .collect(),
        transformer: infos::<TransformerRegistration>(),
        chunker: infos::<ChunkerRegistration>(),
        embedder: infos::<EmbedderRegistration>(),
        vision: infos::<VisionRegistration>(),
        connection_kinds: connection_kinds()
            .into_iter()
            .map(|k| ConnectionKindInfo {
                kind: k.kind,
                label: k.label,
                config_schema: (k.config_schema)(),
                secret_schema: (k.secret_schema)(),
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Self-check
// ---------------------------------------------------------------------------

/// Check registry invariants; returns every problem found:
/// unique names per kind, unique connection kinds, config/secret schemas
/// that reject unknown fields (P-2), and connectors/plugins referencing
/// registered connection kinds. Run from tests (and at startup by the bin).
pub fn check() -> Result<(), Vec<String>> {
    let mut problems = Vec::new();
    check_kind::<ConnectorRegistration>(&mut problems);
    check_kind::<ParserRegistration>(&mut problems);
    check_kind::<TransformerRegistration>(&mut problems);
    check_kind::<ChunkerRegistration>(&mut problems);
    check_kind::<EmbedderRegistration>(&mut problems);
    check_kind::<VisionRegistration>(&mut problems);

    let kinds = connection_kinds();
    for w in kinds.windows(2) {
        if w[0].kind == w[1].kind {
            problems.push(format!("connection kind {:?} registered twice", w[0].kind));
        }
    }
    for k in &kinds {
        if !denies_unknown(&(k.config_schema)()) {
            problems.push(format!(
                "connection kind {:?}: config schema allows unknown fields",
                k.kind
            ));
        }
        if !denies_unknown(&(k.secret_schema)()) {
            problems.push(format!(
                "connection kind {:?}: secret schema allows unknown fields",
                k.kind
            ));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

fn check_kind<R: Registered>(problems: &mut Vec<String>) {
    let regs = all::<R>();
    for w in regs.windows(2) {
        if w[0].meta().name == w[1].meta().name {
            problems.push(format!(
                "{} {:?} registered twice",
                R::KIND,
                w[0].meta().name
            ));
        }
    }
    for r in regs {
        let m = r.meta();
        if !denies_unknown(&m.schema()) {
            problems.push(format!(
                "{} {:?}: config schema allows unknown fields",
                R::KIND,
                m.name
            ));
        }
        for kind in m.connection_kinds {
            if connection_kind(kind).is_none() {
                problems.push(format!(
                    "{} {:?}: connection kind {kind:?} is not registered",
                    R::KIND,
                    m.name
                ));
            }
        }
    }
}

/// Whether an object schema rejects unknown properties
/// (`#[serde(deny_unknown_fields)]` → `additionalProperties: false`).
fn denies_unknown(schema: &Value) -> bool {
    schema.get("additionalProperties") == Some(&Value::Bool(false))
}
