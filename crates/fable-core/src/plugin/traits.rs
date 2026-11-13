//! The plugin runtime traits (spec §8 P-1, P-4, P-7).
//!
//! These are object-safe (`dyn`) so the registry can hand out boxed
//! instances; they use `async_trait` because native `async fn` in traits is
//! not dyn-compatible.

use std::path::{Path, PathBuf};
use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::block::{Block, ChunkDraft, Metadata, ParsedDocument};
use crate::item::ItemRef;
use crate::plugin::PluginError;

/// A boxed, `Send` stream.
pub type BoxStream<'a, T> = Pin<Box<dyn Stream<Item = T> + Send + 'a>>;

/// Item content as a stream of byte chunks (P-4 `fetch`).
pub type ByteStream = BoxStream<'static, Result<Bytes, PluginError>>;

// ---------------------------------------------------------------------------
// Connector
// ---------------------------------------------------------------------------

/// Per-run input to [`Connector::list`].
#[derive(Debug, Clone, Default)]
pub struct ListContext {
    /// The connector-owned cursor persisted on the source after the last
    /// successful listing (`source.state`); `Null` on the first run.
    pub state: Value,
    /// Force resync: incremental connectors should enumerate everything.
    pub force: bool,
}

/// What [`Connector::list`] yields.
#[derive(Debug, Clone, PartialEq)]
pub enum ListEvent {
    /// One item of the source.
    Item(ItemRef),
    /// New cursor state. The sync engine persists the last checkpoint only
    /// after the listing completes successfully (§10.1 step 5).
    Checkpoint(Value),
}

/// Per-call input to [`Connector::fetch`].
#[derive(Debug, Clone, Default)]
pub struct FetchContext {
    /// The source's size cap in bytes. The index job enforces it while
    /// streaming (I-2); connectors may use it to fail early.
    pub max_bytes: Option<u64>,
}

/// Pulls items from an external system (P-4).
///
/// An instance is built per sync/index run from the source's typed config
/// and the **resolved connection** (P-5); connectors never read env vars.
#[async_trait]
pub trait Connector: Send + Sync {
    /// Enumerate the source cheaply, metadata only. Must not download
    /// content (except small `inline` text). Full-enumeration connectors
    /// must yield every existing item, since unseen documents are
    /// tombstoned after a successful listing.
    fn list<'a>(&'a self, ctx: &'a ListContext) -> BoxStream<'a, Result<ListEvent, PluginError>>;

    /// Stream one item's content. Called **only** from the index job.
    ///
    /// The default returns the item's `inline` content and fails otherwise,
    /// which suits inline-only connectors.
    async fn fetch(&self, _ctx: &FetchContext, item: &ItemRef) -> Result<ByteStream, PluginError> {
        match &item.inline {
            Some(text) => Ok(inline_stream(text.clone())),
            None => Err(PluginError::Failed(format!(
                "item {:?} has no inline content and this connector does not fetch",
                item.external_id
            ))),
        }
    }
}

/// A [`ByteStream`] yielding `text` as a single chunk.
pub fn inline_stream(text: String) -> ByteStream {
    Box::pin(Once(Some(Ok(Bytes::from(text)))))
}

struct Once(Option<Result<Bytes, PluginError>>);

impl Stream for Once {
    type Item = Result<Bytes, PluginError>;
    fn poll_next(
        mut self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::task::Poll::Ready(self.0.take())
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Per-document parse options set by the index job from the collection's
/// pipeline (I-5, W-2, X-2). Serialisable: it crosses the parse-worker IPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseOptions {
    /// Directory (inside the job's scratch dir) where image blocks' bytes
    /// are written. `None` = the collection has no vision model; parsers
    /// emit no image blocks.
    #[serde(default)]
    pub image_dir: Option<PathBuf>,
    /// Images smaller than this in both dimensions are skipped (I-5).
    #[serde(default = "default_min_image_px")]
    pub min_image_px: u32,
    /// Maximum image blocks per document (I-5).
    #[serde(default = "default_max_images")]
    pub max_images: u32,
    /// Page limit (W-2); exceeding it fails the document.
    #[serde(default = "default_max_pages")]
    pub max_pages: u32,
}

fn default_min_image_px() -> u32 {
    64
}
fn default_max_images() -> u32 {
    50
}
fn default_max_pages() -> u32 {
    2000
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            image_dir: None,
            min_image_px: default_min_image_px(),
            max_images: default_max_images(),
            max_pages: default_max_pages(),
        }
    }
}

/// Input to [`Parser::parse`]: a file on disk, never an in-memory buffer
/// (P-7). Parsers open it themselves (as a reader or, for pdfium, by path).
#[derive(Debug, Clone, Copy)]
pub struct ParseInput<'a> {
    /// The scratch file holding the fetched content.
    pub path: &'a Path,
    /// MIME type reported by the connector.
    pub mime_type: &'a str,
    /// Per-document options.
    pub options: &'a ParseOptions,
}

/// Document-level results of a parse; the blocks went to the sink.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ParseOutcome {
    /// Title found in the document (e.g. DOCX `docProps/core.xml`).
    #[serde(default)]
    pub title: Option<String>,
    /// Pages or slides processed, when meaningful.
    #[serde(default)]
    pub pages: Option<u32>,
    /// Other document-level metadata.
    #[serde(default)]
    pub metadata: Metadata,
}

/// Receives blocks as a parser produces them, so memory stays flat. The
/// parse worker implements it by writing IPC frames; tests use `Vec<Block>`.
#[async_trait]
pub trait BlockSink: Send {
    /// Accept the next block in reading order.
    async fn push(&mut self, block: Block) -> Result<(), PluginError>;
}

#[async_trait]
impl BlockSink for Vec<Block> {
    async fn push(&mut self, block: Block) -> Result<(), PluginError> {
        Vec::push(self, block);
        Ok(())
    }
}

/// Turns a file into blocks (P-7, §10.5). Runs inside a parse-worker
/// process (§10.4).
#[async_trait]
pub trait Parser: Send + Sync {
    /// Parse `input`, pushing blocks to `sink` in reading order.
    async fn parse(
        &self,
        input: ParseInput<'_>,
        sink: &mut dyn BlockSink,
    ) -> Result<ParseOutcome, PluginError>;
}

// ---------------------------------------------------------------------------
// Transformer, chunker
// ---------------------------------------------------------------------------

/// Cleans up or enriches a parsed document before chunking. Transformers
/// run as an ordered chain; returning no blocks drops the document from the
/// index.
#[async_trait]
pub trait Transformer: Send + Sync {
    /// Transform the document.
    async fn transform(&self, doc: ParsedDocument) -> Result<ParsedDocument, PluginError>;
}

/// Splits a parsed document into non-overlapping chunks (§11, CH-1).
#[async_trait]
pub trait Chunker: Send + Sync {
    /// Chunk the document; ordinals start at 0.
    async fn chunk(&self, doc: &ParsedDocument) -> Result<Vec<ChunkDraft>, PluginError>;
}

// ---------------------------------------------------------------------------
// Embedder, vision
// ---------------------------------------------------------------------------

/// Maps texts to fixed-dimension vectors.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Vector dimension.
    fn dimension(&self) -> usize;
    /// Embed `texts`, returning one vector per text, in order.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PluginError>;
}

/// One image handed to a [`Vision`] model.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageInput {
    /// Image bytes (already size-limited by the pipeline).
    pub bytes: Bytes,
    /// MIME type, e.g. `image/png`.
    pub mime_type: String,
}

/// Describes images as text for indexing (I-5).
#[async_trait]
pub trait Vision: Send + Sync {
    /// One description per image, in order. An empty string means "nothing
    /// useful"; the pipeline then drops the block.
    async fn describe(&self, images: &[ImageInput]) -> Result<Vec<String>, PluginError>;
}
