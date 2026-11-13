//! Compile-time plugin model (spec §8): runtime traits, typed definitions,
//! `inventory`-based registration and the registry.
//!
//! # Writing a plugin
//!
//! 1. Implement the runtime trait ([`Connector`], [`Parser`], [`Transformer`],
//!    [`Chunker`], [`Embedder`] or [`Vision`]).
//! 2. Implement [`PluginDef`] (name, typed config, constructor) and, for
//!    connectors and parsers, [`ConnectorDef`] / [`ParserDef`].
//! 3. Register it with the matching macro, e.g.
//!    `fable_core::register_connector!(MyConnector);`.
//!
//! Connector crates are linked into the `fable` binary behind default-on
//! Cargo features, so disabling a feature removes the connector from the
//! registry. Plugins are looked up through [`registry`].

use std::time::Duration;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod config;
pub mod connection;
pub mod registry;
pub mod traits;

pub use config::{parse_config, ConfigValidationError, EmptyConfig, FieldError};
pub use connection::{ConnectionKindDef, ConnectionKindRegistration, ResolvedConnection};
pub use traits::{
    inline_stream, BlockSink, BoxStream, ByteStream, Chunker, Connector, Embedder, FetchContext,
    ImageInput, ListContext, ListEvent, ParseInput, ParseOptions, ParseOutcome, Parser,
    Transformer, Vision,
};

#[doc(hidden)]
pub mod __private {
    pub use inventory;
}

/// The six plugin kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    /// [`Connector`]
    Connector,
    /// [`Parser`]
    Parser,
    /// [`Transformer`]
    Transformer,
    /// [`Chunker`]
    Chunker,
    /// [`Embedder`]
    Embedder,
    /// [`Vision`]
    Vision,
}

impl PluginKind {
    /// Stable snake_case name.
    pub fn as_str(self) -> &'static str {
        match self {
            PluginKind::Connector => "connector",
            PluginKind::Parser => "parser",
            PluginKind::Transformer => "transformer",
            PluginKind::Chunker => "chunker",
            PluginKind::Embedder => "embedder",
            PluginKind::Vision => "vision",
        }
    }
}

impl std::fmt::Display for PluginKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Errors from building or running plugins. Messages must never contain
/// secrets.
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    /// The plugin config (or source config) is invalid.
    #[error("invalid configuration: {0}")]
    InvalidConfig(#[from] ConfigValidationError),
    /// No plugin of this kind has this name.
    #[error("no {kind} plugin named {name:?}")]
    NotFound {
        /// Plugin kind.
        kind: PluginKind,
        /// Requested name.
        name: String,
    },
    /// No parser handles this MIME type.
    #[error("no parser for MIME type {0:?}")]
    NoParser(String),
    /// The plugin needs a connection of one of `expected` kinds.
    #[error("{plugin} requires a connection of kind {expected:?}{}", got.as_deref().map(|g| format!(", got {g:?}")).unwrap_or_default())]
    Connection {
        /// Plugin name.
        plugin: &'static str,
        /// Accepted connection kinds.
        expected: &'static [&'static str],
        /// The kind supplied, if any.
        got: Option<String>,
    },
    /// The external system asked us to slow down (maps to the queue's
    /// `RateLimited` error).
    #[error("rate limited by the source")]
    RateLimited {
        /// Suggested wait, if the source said.
        retry_after: Option<Duration>,
    },
    /// A per-document limit was exceeded (size cap, page limit, …).
    #[error("limit exceeded: {0}")]
    LimitExceeded(String),
    /// Local I/O failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Any other failure.
    #[error("{0}")]
    Failed(String),
}

/// What a plugin constructor receives besides its typed config.
#[derive(Debug, Clone, Default)]
pub struct BuildContext {
    /// The resolved connection, for plugins that declare
    /// [`PluginDef::CONNECTION_KINDS`].
    pub connection: Option<ResolvedConnection>,
    /// Model name, for embedders and vision plugins.
    pub model: Option<String>,
}

/// Static description and constructor shared by every plugin kind (P-2).
pub trait PluginDef: Sized + Send + Sync + 'static {
    /// Stable name, unique within the plugin kind.
    const NAME: &'static str;
    /// One-line description for the portal.
    const DESCRIPTION: &'static str = "";
    /// Connection kinds this plugin accepts. Empty = needs no connection.
    /// When non-empty, building fails without a connection of one of these
    /// kinds.
    const CONNECTION_KINDS: &'static [&'static str] = &[];
    /// Typed config; use `#[serde(deny_unknown_fields)]`.
    type Config: DeserializeOwned + JsonSchema;
    /// Build an instance from a validated config.
    fn build(config: Self::Config, ctx: BuildContext) -> Result<Self, PluginError>;
}

/// Extra connector metadata (P-6).
pub trait ConnectorDef: PluginDef + Connector {
    /// `list` yields only changed items and tracks deletions via its cursor,
    /// so the sync engine must not tombstone unseen documents.
    const INCREMENTAL: bool = false;
    /// The portal may offer a paste-a-link field ([`Self::parse_target_url`]).
    const SUPPORTS_URL_TARGETS: bool = false;
    /// Turn a pasted URL into a source-config fragment, or `None` if this
    /// connector can't interpret it. Pure: no network.
    fn parse_target_url(_url: &str, _connection: Option<&ResolvedConnection>) -> Option<Value> {
        None
    }
}

/// Extra parser metadata (P-7).
pub trait ParserDef: PluginDef + Parser {
    /// MIME types handled when auto-selecting (lowercase, no parameters).
    const MEDIA_TYPES: &'static [&'static str];
    /// Highest priority wins auto-selection among parsers claiming a type.
    const PRIORITY: i32 = 0;
}

/// Name, description, connection kinds and config schema of a registered
/// plugin; common to every registration.
#[derive(Clone, Copy)]
pub struct PluginMeta {
    /// Stable name.
    pub name: &'static str,
    /// Description.
    pub description: &'static str,
    /// Accepted connection kinds.
    pub connection_kinds: &'static [&'static str],
    /// JSON Schema of the config.
    pub config_schema: fn() -> Value,
    /// Validate a config value (onboarding time).
    pub validate_config: fn(&Value) -> Result<(), ConfigValidationError>,
}

impl PluginMeta {
    /// Meta for `T`.
    pub const fn of<T: PluginDef>() -> Self {
        Self {
            name: T::NAME,
            description: T::DESCRIPTION,
            connection_kinds: T::CONNECTION_KINDS,
            config_schema: config::schema_of::<T::Config>,
            validate_config: validate_config::<T>,
        }
    }

    /// Validate `config` against this plugin's typed config (P-2).
    pub fn validate(&self, config: &Value) -> Result<(), ConfigValidationError> {
        (self.validate_config)(config)
    }

    /// The config JSON Schema.
    pub fn schema(&self) -> Value {
        (self.config_schema)()
    }
}

fn validate_config<T: PluginDef>(v: &Value) -> Result<(), ConfigValidationError> {
    parse_config::<T::Config>(v).map(drop)
}

/// Validate config and connection, then call `T::build`.
fn build_typed<T: PluginDef>(raw: &Value, ctx: BuildContext) -> Result<T, PluginError> {
    let config = parse_config::<T::Config>(raw)?;
    if !T::CONNECTION_KINDS.is_empty() {
        let got = ctx.connection.as_ref().map(|c| c.kind.as_str());
        if !got.is_some_and(|k| T::CONNECTION_KINDS.contains(&k)) {
            return Err(PluginError::Connection {
                plugin: T::NAME,
                expected: T::CONNECTION_KINDS,
                got: got.map(str::to_owned),
            });
        }
    }
    T::build(config, ctx)
}

/// Builds a boxed plugin instance from a raw config.
pub type BuildFn<D> = fn(&Value, BuildContext) -> Result<Box<D>, PluginError>;

/// A registered connector.
pub struct ConnectorRegistration {
    /// Common metadata.
    pub meta: PluginMeta,
    /// See [`ConnectorDef::INCREMENTAL`].
    pub incremental: bool,
    /// See [`ConnectorDef::SUPPORTS_URL_TARGETS`].
    pub supports_url_targets: bool,
    /// See [`ConnectorDef::parse_target_url`].
    pub parse_target_url: fn(&str, Option<&ResolvedConnection>) -> Option<Value>,
    /// Constructor.
    pub build: BuildFn<dyn Connector>,
}

impl ConnectorRegistration {
    /// Registration for `T`.
    pub const fn of<T: ConnectorDef>() -> Self {
        Self {
            meta: PluginMeta::of::<T>(),
            incremental: T::INCREMENTAL,
            supports_url_targets: T::SUPPORTS_URL_TARGETS,
            parse_target_url: T::parse_target_url,
            build: build_connector::<T>,
        }
    }
}

fn build_connector<T: ConnectorDef>(
    raw: &Value,
    ctx: BuildContext,
) -> Result<Box<dyn Connector>, PluginError> {
    Ok(Box::new(build_typed::<T>(raw, ctx)?))
}

/// A registered parser.
pub struct ParserRegistration {
    /// Common metadata.
    pub meta: PluginMeta,
    /// See [`ParserDef::MEDIA_TYPES`].
    pub media_types: &'static [&'static str],
    /// See [`ParserDef::PRIORITY`].
    pub priority: i32,
    /// Constructor.
    pub build: BuildFn<dyn Parser>,
}

impl ParserRegistration {
    /// Registration for `T`.
    pub const fn of<T: ParserDef>() -> Self {
        Self {
            meta: PluginMeta::of::<T>(),
            media_types: T::MEDIA_TYPES,
            priority: T::PRIORITY,
            build: build_parser::<T>,
        }
    }
}

fn build_parser<T: ParserDef>(
    raw: &Value,
    ctx: BuildContext,
) -> Result<Box<dyn Parser>, PluginError> {
    Ok(Box::new(build_typed::<T>(raw, ctx)?))
}

macro_rules! simple_registration {
    ($reg:ident, $trait:ident, $build:ident, $doc:literal) => {
        #[doc = $doc]
        pub struct $reg {
            /// Common metadata.
            pub meta: PluginMeta,
            /// Constructor.
            pub build: BuildFn<dyn $trait>,
        }

        impl $reg {
            /// Registration for `T`.
            pub const fn of<T: PluginDef + $trait>() -> Self {
                Self {
                    meta: PluginMeta::of::<T>(),
                    build: $build::<T>,
                }
            }
        }

        fn $build<T: PluginDef + $trait>(
            raw: &Value,
            ctx: BuildContext,
        ) -> Result<Box<dyn $trait>, PluginError> {
            Ok(Box::new(build_typed::<T>(raw, ctx)?))
        }
    };
}

simple_registration!(
    TransformerRegistration,
    Transformer,
    build_transformer,
    "A registered transformer."
);
simple_registration!(
    ChunkerRegistration,
    Chunker,
    build_chunker,
    "A registered chunker."
);
simple_registration!(
    EmbedderRegistration,
    Embedder,
    build_embedder,
    "A registered embedder."
);
simple_registration!(
    VisionRegistration,
    Vision,
    build_vision,
    "A registered vision plugin."
);

inventory::collect!(ConnectorRegistration);
inventory::collect!(ParserRegistration);
inventory::collect!(TransformerRegistration);
inventory::collect!(ChunkerRegistration);
inventory::collect!(EmbedderRegistration);
inventory::collect!(VisionRegistration);
inventory::collect!(ConnectionKindRegistration);

/// Register a connector type implementing [`ConnectorDef`].
#[macro_export]
macro_rules! register_connector {
    ($t:ty) => {
        $crate::plugin::__private::inventory::submit! {
            $crate::plugin::ConnectorRegistration::of::<$t>()
        }
    };
}

/// Register a parser type implementing [`ParserDef`].
#[macro_export]
macro_rules! register_parser {
    ($t:ty) => {
        $crate::plugin::__private::inventory::submit! {
            $crate::plugin::ParserRegistration::of::<$t>()
        }
    };
}

/// Register a transformer type implementing [`PluginDef`] + [`Transformer`].
#[macro_export]
macro_rules! register_transformer {
    ($t:ty) => {
        $crate::plugin::__private::inventory::submit! {
            $crate::plugin::TransformerRegistration::of::<$t>()
        }
    };
}

/// Register a chunker type implementing [`PluginDef`] + [`Chunker`].
#[macro_export]
macro_rules! register_chunker {
    ($t:ty) => {
        $crate::plugin::__private::inventory::submit! {
            $crate::plugin::ChunkerRegistration::of::<$t>()
        }
    };
}

/// Register an embedder type implementing [`PluginDef`] + [`Embedder`].
#[macro_export]
macro_rules! register_embedder {
    ($t:ty) => {
        $crate::plugin::__private::inventory::submit! {
            $crate::plugin::EmbedderRegistration::of::<$t>()
        }
    };
}

/// Register a vision type implementing [`PluginDef`] + [`Vision`].
#[macro_export]
macro_rules! register_vision {
    ($t:ty) => {
        $crate::plugin::__private::inventory::submit! {
            $crate::plugin::VisionRegistration::of::<$t>()
        }
    };
}

/// Register a connection kind implementing [`ConnectionKindDef`].
#[macro_export]
macro_rules! register_connection_kind {
    ($t:ty) => {
        $crate::plugin::__private::inventory::submit! {
            $crate::plugin::ConnectionKindRegistration::of::<$t>()
        }
    };
}
