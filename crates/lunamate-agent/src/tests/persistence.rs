//! 提供不依赖具体数据库引擎的 Agent 持久化测试 fake。

use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;

use crate::persistence::{
    AgentPersistenceCallbacks, PersistenceError, PersistentMemoryTier, PersistentMemoryUsage,
    SessionDocument,
};

const SESSION_KEY_PREFIX: &str = "chat-session/";

#[derive(Default)]
struct TestState {
    sessions: HashMap<String, SessionDocument>,
    memories: HashMap<(String, PersistentMemoryTier), u64>,
}

#[derive(Clone, Default)]
pub(super) struct TestDatabase {
    state: Arc<Mutex<TestState>>,
}

impl TestDatabase {
    pub(super) async fn open_memory() -> Result<Arc<Self>, PersistenceError> {
        Ok(Arc::new(Self::default()))
    }

    pub(super) fn callbacks(self: &Arc<Self>) -> AgentPersistenceCallbacks {
        let load = self.clone();
        let save = self.clone();
        let delete = self.clone();
        let usage = self.clone();
        let clear = self.clone();
        AgentPersistenceCallbacks::new(
            move |persona| {
                let database = load.clone();
                async move { Ok(database.state.lock().sessions.get(&persona).cloned()) }
            },
            move |persona, document| {
                let database = save.clone();
                async move {
                    database.state.lock().sessions.insert(persona, document);
                    Ok(())
                }
            },
            move |persona| {
                let database = delete.clone();
                async move {
                    database.state.lock().sessions.remove(&persona);
                    Ok(())
                }
            },
            move |persona| {
                let database = usage.clone();
                async move {
                    let state = database.state.lock();
                    Ok(PersistentMemoryUsage::new(
                        state
                            .memories
                            .get(&(persona.clone(), PersistentMemoryTier::Medium))
                            .copied()
                            .unwrap_or(0),
                        state
                            .memories
                            .get(&(persona, PersistentMemoryTier::Long))
                            .copied()
                            .unwrap_or(0),
                    ))
                }
            },
            move |persona, tier| {
                let database = clear.clone();
                async move {
                    let mut state = database.state.lock();
                    match tier {
                        Some(tier) => {
                            state.memories.remove(&(persona, tier));
                        }
                        None => state.memories.retain(|(agent, _), _| agent != &persona),
                    }
                    Ok(())
                }
            },
        )
    }

    pub(super) async fn write_document(
        &self,
        _scope: &str,
        key: &str,
        format_version: u32,
        contents: &[u8],
    ) -> Result<(), PersistenceError> {
        let persona = persona_from_key(key)?;
        self.state.lock().sessions.insert(
            persona.to_owned(),
            SessionDocument::new(format_version, contents.to_vec()),
        );
        Ok(())
    }

    pub(super) async fn read_document(
        &self,
        _scope: &str,
        key: &str,
    ) -> Result<Option<SessionDocument>, PersistenceError> {
        let persona = persona_from_key(key)?;
        Ok(self.state.lock().sessions.get(persona).cloned())
    }

    pub(super) async fn insert_memory_for_test(
        &self,
        persona: &str,
        tier: PersistentMemoryTier,
        valid: bool,
    ) -> Result<(), PersistenceError> {
        if valid {
            *self
                .state
                .lock()
                .memories
                .entry((persona.to_owned(), tier))
                .or_default() += 1;
        }
        Ok(())
    }

    pub(super) async fn agent_memory_usage(
        self: &Arc<Self>,
        persona: &str,
    ) -> Result<PersistentMemoryUsage, PersistenceError> {
        self.callbacks().memory_usage(persona).await
    }
}

fn persona_from_key(key: &str) -> Result<&str, PersistenceError> {
    key.strip_prefix(SESSION_KEY_PREFIX)
        .filter(|persona| !persona.is_empty())
        .ok_or_else(|| PersistenceError::new("invalid_test_key", "无效测试会话键"))
}
