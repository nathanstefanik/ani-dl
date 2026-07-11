//! Core-side material cache keyed by source.

use std::collections::HashMap;

use tokio::sync::Mutex;

use crate::minter::material::Material;

#[derive(Debug, Default)]
pub struct MaterialCache {
    inner: Mutex<HashMap<String, Material>>,
}

impl MaterialCache {
    pub async fn get(&self, source: &str) -> Option<Material> {
        let now = chrono::Utc::now().timestamp_millis();
        let guard = self.inner.lock().await;
        guard
            .get(source)
            .filter(|m| m.is_fresh(now))
            .cloned()
    }

    pub async fn set(&self, source: &str, material: Material) {
        self.inner
            .lock()
            .await
            .insert(source.to_string(), material);
    }

    pub async fn invalidate(&self, source: &str) {
        self.inner.lock().await.remove(source);
    }
}
