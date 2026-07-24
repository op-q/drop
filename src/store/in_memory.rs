use std::{collections::HashMap, sync::Arc};

use tokio::sync::Mutex;

use crate::domain::session::Session;

#[derive(Clone, Default)]
pub struct InMemorySessionStore {
    inner: Arc<Mutex<HashMap<String, Session>>>,
}

impl InMemorySessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.inner.lock().await.is_empty()
    }

    pub async fn insert(&self, code: String, session: Session) {
        self.inner.lock().await.insert(code, session);
    }

    pub async fn get(&self, code: &str) -> Option<Session> {
        self.inner.lock().await.get(code).cloned()
    }

    pub async fn remove(&self, code: &str) -> Option<Session> {
        self.inner.lock().await.remove(code)
    }

    pub async fn contains(&self, code: &str) -> bool {
        self.inner.lock().await.contains_key(code)
    }

    pub async fn with_session_mut<R>(
        &self,
        code: &str,
        update: impl FnOnce(&mut Session) -> R,
    ) -> Option<R> {
        let mut sessions = self.inner.lock().await;
        sessions.get_mut(code).map(update)
    }

    pub async fn with_all_mut<R>(
        &self,
        update: impl FnOnce(&mut HashMap<String, Session>) -> R,
    ) -> R {
        let mut sessions = self.inner.lock().await;
        update(&mut sessions)
    }
}
