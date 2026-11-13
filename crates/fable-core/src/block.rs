//! The parsed-document model shared by parsers, transformers, the chunker
//! and the parse-worker IPC (spec §8, §10.4, §10.5, §11).
//!
//! A parser turns a source file into a sequence of [`Block`]s; transformers
//! rewrite a [`ParsedDocument`]; the chunker cuts it into [`ChunkDraft`]s,
//! each carrying a block map ([`BlockSpan`]) so a document can be re-chunked
//! from stored chunks without fetching the source again (CH-8).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Free-form, JSON-serialisable metadata attached to blocks, documents and
/// chunks.
pub type Metadata = Map<String, Value>;

/// What a block represents. Stored as snake_case text, never a Postgres enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockType {
    /// A heading; [`Block::level`] holds its depth (1 = top level).
    Heading,
    /// Body text.
    #[default]
    Paragraph,
    /// Preformatted code (e.g. a Confluence code macro, X-6).
    Code,
    /// One table row as `header: value` pairs (X-4, X-6, X-7).
    TableRow,
    /// One item of a bulleted or numbered list.
    ListItem,
    /// An image to be described by the vision model (I-5). [`Block::image`]
    /// points at its bytes; after the vision step the block's `text` holds
    /// the `[Figure: …]` description.
    Image,
}

impl BlockType {
    /// Stable snake_case name, as serialised.
    pub fn as_str(self) -> &'static str {
        match self {
            BlockType::Heading => "heading",
            BlockType::Paragraph => "paragraph",
            BlockType::Code => "code",
            BlockType::TableRow => "table_row",
            BlockType::ListItem => "list_item",
            BlockType::Image => "image",
        }
    }
}

/// Where an image block's bytes live.
///
/// Image bytes never travel inside a block: the parser writes them to a file
/// in the job's scratch directory (managed by the worker's scratch guard and
/// deleted with it), and the block only carries this reference. This keeps
/// the parse-worker IPC frames small and memory flat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageRef {
    /// Path of the image file in the job's scratch directory.
    pub path: PathBuf,
    /// MIME type of the file, e.g. `image/png`.
    pub mime_type: String,
    /// Hex sha256 of the image bytes; the `vision_cache` key (I-5).
    pub sha256: String,
    /// Pixel width, when the parser knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// Pixel height, when the parser knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

/// One structural unit of a parsed document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Block {
    /// Block kind.
    #[serde(rename = "type", default)]
    pub block_type: BlockType,
    /// The block's text. Empty for an image block until the vision step
    /// fills in its description.
    #[serde(default)]
    pub text: String,
    /// Heading depth for [`BlockType::Heading`] (1-based); 0 otherwise.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub level: u8,
    /// 1-based PDF page the block comes from (X-1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    /// 1-based PPTX slide the block comes from (X-5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slide: Option<u32>,
    /// Image bytes reference for [`BlockType::Image`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<ImageRef>,
    /// Parser-specific extras.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Metadata,
}

fn is_zero(v: &u8) -> bool {
    *v == 0
}

impl Block {
    /// A block of the given type with text and no extras.
    pub fn new(block_type: BlockType, text: impl Into<String>) -> Self {
        Self {
            block_type,
            text: text.into(),
            level: 0,
            page: None,
            slide: None,
            image: None,
            metadata: Metadata::new(),
        }
    }

    /// A paragraph.
    pub fn paragraph(text: impl Into<String>) -> Self {
        Self::new(BlockType::Paragraph, text)
    }

    /// A heading at `level` (1 = top level).
    pub fn heading(level: u8, text: impl Into<String>) -> Self {
        Self {
            level,
            ..Self::new(BlockType::Heading, text)
        }
    }

    /// An image block referencing bytes in scratch.
    pub fn image(image: ImageRef) -> Self {
        Self {
            image: Some(image),
            ..Self::new(BlockType::Image, String::new())
        }
    }

    /// Set the source page.
    pub fn with_page(mut self, page: u32) -> Self {
        self.page = Some(page);
        self
    }

    /// Set the source slide.
    pub fn with_slide(mut self, slide: u32) -> Self {
        self.slide = Some(slide);
        self
    }
}

/// A whole parsed document: what transformers and the chunker work on.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ParsedDocument {
    /// Document title (from the source or the parser), may be empty.
    #[serde(default)]
    pub title: String,
    /// Blocks in reading order.
    #[serde(default)]
    pub blocks: Vec<Block>,
    /// Document-level metadata.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Metadata,
}

/// One entry of a chunk's block map (CH-8, stored in `chunks.blocks`):
/// where a block sits inside the chunk's text and what it was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockSpan {
    /// Character offset of the block inside the chunk text.
    pub offset: u32,
    /// Length of the block's text in characters.
    pub len: u32,
    /// Block kind.
    #[serde(rename = "type")]
    pub block_type: BlockType,
    /// Heading depth, for headings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    /// Source page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    /// Source slide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slide: Option<u32>,
}

/// A chunk produced by a chunker, before context and embedding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkDraft {
    /// 0-based position of the chunk within the document.
    pub ordinal: u32,
    /// Source text only: no breadcrumb or context prefix (CH-4).
    pub text: String,
    /// Heading breadcrumb, outermost first (`metadata.heading_path`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub heading_path: Vec<String>,
    /// Block map (CH-8).
    #[serde(default)]
    pub blocks: Vec<BlockSpan>,
    /// Other chunk metadata (e.g. `figures`, CH-7).
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Metadata,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn block_serialises_compactly_with_snake_case_type() {
        let b = Block::heading(2, "Setup").with_page(3);
        assert_eq!(
            serde_json::to_value(&b).unwrap(),
            json!({"type": "heading", "text": "Setup", "level": 2, "page": 3})
        );
        let p = Block::new(BlockType::TableRow, "Env: prod");
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            json!({"type": "table_row", "text": "Env: prod"})
        );
    }

    #[test]
    fn block_round_trips_with_image_ref() {
        let b = Block::image(ImageRef {
            path: PathBuf::from("/scratch/job/img-1.png"),
            mime_type: "image/png".into(),
            sha256: "ab".repeat(32),
            width: Some(640),
            height: None,
        })
        .with_slide(4);
        let s = serde_json::to_string(&b).unwrap();
        let back: Block = serde_json::from_str(&s).unwrap();
        assert_eq!(back, b);
        assert_eq!(back.block_type.as_str(), "image");
    }

    #[test]
    fn block_defaults_to_paragraph() {
        let b: Block = serde_json::from_value(json!({"text": "x"})).unwrap();
        assert_eq!(b, Block::paragraph("x"));
    }
}
