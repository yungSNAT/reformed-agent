//! Lightweight sticker metadata cache.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone)]
pub struct StickerMeta {
    pub id: String,
    pub name: String,
    pub mime_type: Option<String>,
}

#[derive(Clone, Default)]
pub struct StickerCache {
    entries: Arc<RwLock<HashMap<String, StickerMeta>>>,
}

impl StickerCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&self, meta: StickerMeta) {
        if let Ok(mut entries) = self.entries.write() {
            entries.insert(meta.id.clone(), meta);
        }
    }

    pub fn get(&self, id: &str) -> Option<StickerMeta> {
        self.entries.read().ok().and_then(|m| m.get(id).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::{StickerCache, StickerMeta};

    #[test]
    fn cloned_caches_share_metadata_and_replace_an_existing_sticker() {
        let cache = StickerCache::new();
        let second = cache.clone();
        assert!(second.get("fixture").is_none());

        cache.put(StickerMeta {
            id: "fixture".into(),
            name: "Star".into(),
            mime_type: Some("image/png".into()),
        });
        let stored = second.get("fixture").unwrap();
        assert_eq!(stored.name, "Star");
        assert_eq!(stored.mime_type.as_deref(), Some("image/png"));

        second.put(StickerMeta {
            id: "fixture".into(),
            name: "Moon".into(),
            mime_type: None,
        });
        let replaced = cache.get("fixture").unwrap();
        assert_eq!(replaced.id, "fixture");
        assert_eq!(replaced.name, "Moon");
        assert!(replaced.mime_type.is_none());
        assert!(cache.get("unknown").is_none());
    }
}
