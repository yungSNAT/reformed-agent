//! Message mirroring utilities.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Stores mirror routes between source and destination channels.
#[derive(Clone, Default)]
pub struct MirrorManager {
    routes: Arc<RwLock<HashMap<String, String>>>,
}

impl MirrorManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_route(&self, source_channel: impl Into<String>, target_channel: impl Into<String>) {
        if let Ok(mut routes) = self.routes.write() {
            routes.insert(source_channel.into(), target_channel.into());
        }
    }

    pub fn remove_route(&self, source_channel: &str) {
        if let Ok(mut routes) = self.routes.write() {
            routes.remove(source_channel);
        }
    }

    pub fn route_for(&self, source_channel: &str) -> Option<String> {
        self.routes
            .read()
            .ok()
            .and_then(|r| r.get(source_channel).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::MirrorManager;

    #[test]
    fn cloned_managers_share_route_replacements_and_removals() {
        let manager = MirrorManager::new();
        let second = manager.clone();
        assert_eq!(second.route_for("source"), None);

        manager.set_route("source", "first-target");
        manager.set_route("other", "independent-target");
        assert_eq!(second.route_for("source").as_deref(), Some("first-target"));

        second.set_route("source", "next-target");
        assert_eq!(manager.route_for("source").as_deref(), Some("next-target"));
        second.remove_route("source");
        assert_eq!(manager.route_for("source"), None);
        assert_eq!(
            manager.route_for("other").as_deref(),
            Some("independent-target")
        );
    }
}
