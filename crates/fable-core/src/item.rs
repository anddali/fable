//! What a connector's `list()` yields (spec §8 P-4).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Map;

use crate::block::Metadata;

/// A cheap, metadata-only reference to one item in a source.
///
/// The sync engine compares `source_version` with the stored one to decide
/// whether the item needs indexing (§10.1); content is fetched later, only by
/// the index job, through `Connector::fetch` — unless `inline` is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemRef {
    /// Stable identifier of the item within its source.
    pub external_id: String,
    /// Display title.
    #[serde(default)]
    pub title: String,
    /// Link back to the item in the source system.
    #[serde(default)]
    pub uri: String,
    /// MIME type of the content `fetch` will return; drives parser selection.
    pub mime_type: String,
    /// The connector's change marker (page version, eTag, blob SHA, …).
    pub source_version: String,
    /// Content size in bytes, when known up front (lets the size cap skip
    /// an item before fetching it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Last modification time in the source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_updated_at: Option<DateTime<Utc>>,
    /// Connector-specific metadata, stored on the document.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Metadata,
    /// Small text content delivered with the listing (e.g. Slack digests),
    /// so the index job doesn't call `fetch`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline: Option<String>,
}

impl ItemRef {
    /// An item with the required fields set and everything else empty.
    pub fn new(
        external_id: impl Into<String>,
        mime_type: impl Into<String>,
        source_version: impl Into<String>,
    ) -> Self {
        Self {
            external_id: external_id.into(),
            title: String::new(),
            uri: String::new(),
            mime_type: mime_type.into(),
            source_version: source_version.into(),
            size: None,
            source_updated_at: None,
            metadata: Metadata::new(),
            inline: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn item_ref_round_trips_and_omits_empty_optionals() {
        let mut item = ItemRef::new("page-1", "text/html", "7");
        item.title = "Runbook".into();
        let v = serde_json::to_value(&item).unwrap();
        assert_eq!(
            v,
            json!({"external_id": "page-1", "title": "Runbook", "uri": "",
                   "mime_type": "text/html", "source_version": "7"})
        );
        let back: ItemRef = serde_json::from_value(v).unwrap();
        assert_eq!(back, item);
    }
}
