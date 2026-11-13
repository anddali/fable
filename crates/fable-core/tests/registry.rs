//! Registers sample plugins through the public macros (as plugin crates do)
//! and exercises the registry. `forbid(unsafe_code)` proves registration
//! works in our crates.

#![forbid(unsafe_code)]

use async_trait::async_trait;
use fable_core::plugin::registry::{self, catalog, select_parser};
use fable_core::plugin::{
    BlockSink, BuildContext, ChunkerRegistration, ConnectionKindDef, Connector, ConnectorDef,
    ConnectorRegistration, Embedder, EmbedderRegistration, EmptyConfig, FetchContext, ListContext,
    ListEvent, ParseInput, ParseOptions, ParseOutcome, Parser, ParserDef, PluginDef, PluginError,
    PluginKind, ResolvedConnection, Transformer, TransformerRegistration,
};
use fable_core::{Block, BlockType, ChunkDraft, ItemRef, ParsedDocument};
use futures::StreamExt;
use schemars::JsonSchema;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{json, Value};

// --- connection kind --------------------------------------------------------

struct DemoKind;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct DemoKindConfig {
    host: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct DemoKindSecret {
    token: String,
}

impl ConnectionKindDef for DemoKind {
    const KIND: &'static str = "demo";
    const LABEL: &'static str = "Demo service";
    type Config = DemoKindConfig;
    type Secret = DemoKindSecret;
}
fable_core::register_connection_kind!(DemoKind);

// --- connector ----------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DemoConnectorConfig {
    items: u32,
}

struct DemoConnector {
    items: u32,
    token_len: usize,
}

impl PluginDef for DemoConnector {
    const NAME: &'static str = "demo";
    const DESCRIPTION: &'static str = "Lists N inline items";
    const CONNECTION_KINDS: &'static [&'static str] = &["demo"];
    type Config = DemoConnectorConfig;
    fn build(config: Self::Config, ctx: BuildContext) -> Result<Self, PluginError> {
        let token_len = ctx
            .connection
            .and_then(|c| c.secret)
            .map(|s| s.expose_secret().len())
            .unwrap_or(0);
        Ok(Self {
            items: config.items,
            token_len,
        })
    }
}

impl ConnectorDef for DemoConnector {
    const SUPPORTS_URL_TARGETS: bool = true;
    fn parse_target_url(url: &str, _c: Option<&ResolvedConnection>) -> Option<Value> {
        url.strip_prefix("demo://").map(|t| json!({ "target": t }))
    }
}

#[async_trait]
impl Connector for DemoConnector {
    fn list<'a>(
        &'a self,
        _ctx: &'a ListContext,
    ) -> fable_core::plugin::BoxStream<'a, Result<ListEvent, PluginError>> {
        let mut events: Vec<Result<ListEvent, PluginError>> = (0..self.items)
            .map(|i| {
                let mut item = ItemRef::new(format!("item-{i}"), "text/plain", "1");
                item.inline = Some(format!("body {i} ({} byte token)", self.token_len));
                Ok(ListEvent::Item(item))
            })
            .collect();
        events.push(Ok(ListEvent::Checkpoint(json!({"cursor": self.items}))));
        futures::stream::iter(events).boxed()
    }
}
fable_core::register_connector!(DemoConnector);

// --- parsers --------------------------------------------------------------------

struct LinesParser;

impl PluginDef for LinesParser {
    const NAME: &'static str = "lines";
    type Config = EmptyConfig;
    fn build(_: EmptyConfig, _: BuildContext) -> Result<Self, PluginError> {
        Ok(Self)
    }
}
impl ParserDef for LinesParser {
    const MEDIA_TYPES: &'static [&'static str] = &["text/x-lines", "text/markdown"];
    const PRIORITY: i32 = 10;
}

#[async_trait]
impl Parser for LinesParser {
    async fn parse(
        &self,
        input: ParseInput<'_>,
        sink: &mut dyn BlockSink,
    ) -> Result<ParseOutcome, PluginError> {
        let text = tokio::fs::read_to_string(input.path).await?;
        for line in text.lines().filter(|l| !l.is_empty()) {
            let block = match line.strip_prefix("# ") {
                Some(h) => Block::heading(1, h),
                None => Block::paragraph(line),
            };
            sink.push(block).await?;
        }
        Ok(ParseOutcome::default())
    }
}
fable_core::register_parser!(LinesParser);

struct TextParser;
impl PluginDef for TextParser {
    const NAME: &'static str = "text";
    type Config = EmptyConfig;
    fn build(_: EmptyConfig, _: BuildContext) -> Result<Self, PluginError> {
        Ok(Self)
    }
}
impl ParserDef for TextParser {
    const MEDIA_TYPES: &'static [&'static str] = &["text/plain", "text/markdown"];
}
#[async_trait]
impl Parser for TextParser {
    async fn parse(
        &self,
        _: ParseInput<'_>,
        _: &mut dyn BlockSink,
    ) -> Result<ParseOutcome, PluginError> {
        Ok(ParseOutcome::default())
    }
}
fable_core::register_parser!(TextParser);

// --- transformer, chunker, embedder ----------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UpperConfig {
    #[serde(default)]
    headings_only: bool,
}

struct Upper(UpperConfig);
impl PluginDef for Upper {
    const NAME: &'static str = "upper";
    type Config = UpperConfig;
    fn build(c: UpperConfig, _: BuildContext) -> Result<Self, PluginError> {
        Ok(Self(c))
    }
}
#[async_trait]
impl Transformer for Upper {
    async fn transform(&self, mut doc: ParsedDocument) -> Result<ParsedDocument, PluginError> {
        for b in &mut doc.blocks {
            if !self.0.headings_only || b.block_type == BlockType::Heading {
                b.text = b.text.to_uppercase();
            }
        }
        Ok(doc)
    }
}
fable_core::register_transformer!(Upper);

struct OnePerBlock;
impl PluginDef for OnePerBlock {
    const NAME: &'static str = "one-per-block";
    type Config = EmptyConfig;
    fn build(_: EmptyConfig, _: BuildContext) -> Result<Self, PluginError> {
        Ok(Self)
    }
}
#[async_trait]
impl fable_core::plugin::Chunker for OnePerBlock {
    async fn chunk(&self, doc: &ParsedDocument) -> Result<Vec<ChunkDraft>, PluginError> {
        Ok(doc
            .blocks
            .iter()
            .enumerate()
            .map(|(i, b)| ChunkDraft {
                ordinal: i as u32,
                text: b.text.clone(),
                heading_path: vec![],
                blocks: vec![],
                metadata: Default::default(),
            })
            .collect())
    }
}
fable_core::register_chunker!(OnePerBlock);

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LenConfig {
    dim: usize,
}
struct LenEmbedder(usize);
impl PluginDef for LenEmbedder {
    const NAME: &'static str = "len";
    type Config = LenConfig;
    fn build(c: LenConfig, _: BuildContext) -> Result<Self, PluginError> {
        Ok(Self(c.dim))
    }
}
#[async_trait]
impl Embedder for LenEmbedder {
    fn dimension(&self) -> usize {
        self.0
    }
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PluginError> {
        Ok(texts.iter().map(|t| vec![t.len() as f32; self.0]).collect())
    }
}
fable_core::register_embedder!(LenEmbedder);

// --- tests ------------------------------------------------------------------------

fn demo_connection(kind: &str) -> ResolvedConnection {
    ResolvedConnection {
        id: "c1".into(),
        kind: kind.into(),
        name: "Demo".into(),
        config: json!({"host": "example.test"}),
        secret: Some(SecretString::from("s3cret")),
        enabled: true,
        last_test_ok: None,
    }
}

#[test]
fn registry_invariants_hold() {
    registry::check().unwrap();
}

#[tokio::test]
async fn connector_lists_and_fetches_inline() {
    let ctx = BuildContext {
        connection: Some(demo_connection("demo")),
        model: None,
    };
    let c = registry::connector("demo", &json!({"items": 2}), ctx).unwrap();
    let lctx = ListContext::default();
    let events: Vec<_> = c.list(&lctx).collect().await;
    assert_eq!(events.len(), 3);
    let ListEvent::Item(item) = events[0].as_ref().unwrap() else {
        panic!("expected item")
    };
    assert!(matches!(events[2], Ok(ListEvent::Checkpoint(_))));

    let mut bytes = c.fetch(&FetchContext::default(), item).await.unwrap();
    let chunk = bytes.next().await.unwrap().unwrap();
    assert_eq!(&chunk[..], b"body 0 (6 byte token)");
    assert!(bytes.next().await.is_none());

    let no_inline = ItemRef::new("x", "text/plain", "1");
    assert!(c.fetch(&FetchContext::default(), &no_inline).await.is_err());
}

#[test]
fn connector_requires_matching_connection() {
    let err = registry::connector("demo", &json!({"items": 1}), BuildContext::default())
        .err()
        .unwrap();
    assert!(matches!(err, PluginError::Connection { got: None, .. }));

    let ctx = BuildContext {
        connection: Some(demo_connection("slack")),
        model: None,
    };
    let err = registry::connector("demo", &json!({"items": 1}), ctx)
        .err()
        .unwrap();
    assert!(err.to_string().contains("got \"slack\""), "{err}");
}

#[test]
fn config_is_validated_at_onboarding() {
    registry::validate::<ConnectorRegistration>("demo", &json!({"items": 3})).unwrap();
    let err = registry::validate::<ConnectorRegistration>("demo", &json!({"items": 3, "extra": 1}))
        .unwrap_err();
    let PluginError::InvalidConfig(v) = err else {
        panic!("expected InvalidConfig")
    };
    assert!(v.errors[0].message.contains("unknown field `extra`"));

    let err =
        registry::validate::<ConnectorRegistration>("demo", &json!({"items": "x"})).unwrap_err();
    assert!(err.to_string().contains("items"), "{err}");

    let err = registry::validate::<ConnectorRegistration>("nope", &json!({})).unwrap_err();
    assert!(matches!(
        err,
        PluginError::NotFound {
            kind: PluginKind::Connector,
            ..
        }
    ));
}

#[test]
fn connection_kind_validation_hides_secret_values() {
    let k = registry::connection_kind("demo").unwrap();
    (k.validate_config)(&json!({"host": "h"})).unwrap();
    let err = (k.validate_secret)(&json!({"token": 12345})).unwrap_err();
    assert!(!err.to_string().contains("12345"));
}

#[test]
fn url_targets() {
    let reg = registry::get::<ConnectorRegistration>("demo").unwrap();
    assert!(reg.supports_url_targets);
    assert_eq!(
        (reg.parse_target_url)("demo://space/1", None),
        Some(json!({"target": "space/1"}))
    );
    assert_eq!((reg.parse_target_url)("https://x", None), None);
}

#[test]
fn parser_auto_selection_by_mime_and_priority() {
    assert_eq!(select_parser("text/x-lines").unwrap().meta.name, "lines");
    // Both claim markdown; higher priority wins. Params and case ignored.
    assert_eq!(
        select_parser("Text/Markdown; charset=utf-8")
            .unwrap()
            .meta
            .name,
        "lines"
    );
    assert_eq!(select_parser("text/plain").unwrap().meta.name, "text");
    // Unclaimed text/* falls back to `text`.
    assert_eq!(select_parser("text/csv").unwrap().meta.name, "text");
    assert!(matches!(
        select_parser("application/x-unknown"),
        Err(PluginError::NoParser(m)) if m == "application/x-unknown"
    ));
}

#[tokio::test]
async fn parse_transform_chunk_embed_from_a_file() {
    let dir = std::env::temp_dir().join(format!("fable-core-registry-{}", std::process::id()));
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let path = dir.join("doc.txt");
    tokio::fs::write(&path, "# Intro\nhello\n\nworld\n")
        .await
        .unwrap();

    let parser = registry::parser("lines", &Value::Null, BuildContext::default()).unwrap();
    let mut blocks: Vec<Block> = Vec::new();
    let options = ParseOptions::default();
    parser
        .parse(
            ParseInput {
                path: &path,
                mime_type: "text/x-lines",
                options: &options,
            },
            &mut blocks,
        )
        .await
        .unwrap();
    tokio::fs::remove_dir_all(&dir).await.unwrap();
    assert_eq!(blocks[0], Block::heading(1, "Intro"));
    assert_eq!(blocks.len(), 3);

    let t = registry::transformer(
        "upper",
        &json!({"headings_only": true}),
        BuildContext::default(),
    )
    .unwrap();
    let doc = t
        .transform(ParsedDocument {
            title: "t".into(),
            blocks,
            metadata: Default::default(),
        })
        .await
        .unwrap();
    assert_eq!(doc.blocks[0].text, "INTRO");
    assert_eq!(doc.blocks[1].text, "hello");

    let chunker = registry::chunker("one-per-block", &json!({}), BuildContext::default()).unwrap();
    let chunks = chunker.chunk(&doc).await.unwrap();
    assert_eq!(chunks.len(), 3);

    let e = registry::embedder("len", &json!({"dim": 4}), BuildContext::default()).unwrap();
    assert_eq!(e.dimension(), 4);
    let v = e.embed(&[chunks[1].text.clone()]).await.unwrap();
    assert_eq!(v, vec![vec![5.0; 4]]);
}

#[test]
fn catalog_lists_plugins_with_schemas() {
    let c = catalog();
    let v = serde_json::to_value(&c).unwrap();

    let conn = &v["connector"][0];
    assert_eq!(conn["name"], "demo");
    assert_eq!(conn["connection_kinds"], json!(["demo"]));
    assert_eq!(conn["supports_url_targets"], true);
    assert_eq!(conn["incremental"], false);
    assert_eq!(conn["config_schema"]["additionalProperties"], false);
    assert!(conn["config_schema"]["properties"]["items"].is_object());

    let names: Vec<_> = c.parser.iter().map(|p| p.info.name).collect();
    assert_eq!(names, ["lines", "text"]);
    assert_eq!(v["parser"][0]["priority"], 10);
    assert_eq!(v["transformer"][0]["name"], "upper");
    assert_eq!(v["chunker"][0]["name"], "one-per-block");
    assert_eq!(v["embedder"][0]["name"], "len");
    assert_eq!(v["vision"], json!([]));

    let kind = &v["connection_kinds"][0];
    assert_eq!(kind["kind"], "demo");
    assert_eq!(kind["label"], "Demo service");
    assert!(kind["secret_schema"]["properties"]["token"].is_object());

    // Lookups by name for every kind.
    registry::get::<TransformerRegistration>("upper").unwrap();
    registry::get::<ChunkerRegistration>("one-per-block").unwrap();
    registry::get::<EmbedderRegistration>("len").unwrap();
}
