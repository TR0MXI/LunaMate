//! 验证人格记忆句柄只承诺清理数据库中的中期与长期层级。

use crate::{AgentMemory, memory::PersistentMemoryScope, persistence::PersistentMemoryTier};

use super::persistence::TestDatabase;

#[tokio::test]
async fn persistent_memory_scopes_map_to_only_persistent_tiers() {
    let database = TestDatabase::open_memory()
        .await
        .expect("内存数据库应可打开");
    for tier in [PersistentMemoryTier::Medium, PersistentMemoryTier::Long] {
        database
            .insert_memory_for_test("persona", tier, true)
            .await
            .expect("测试记忆应可写入");
    }
    let memory = AgentMemory::new(Some(database.callbacks())).persona("persona");

    memory
        .clear(PersistentMemoryScope::Medium)
        .await
        .expect("中期记忆应可单独清理");
    let usage = database
        .agent_memory_usage("persona")
        .await
        .expect("测试记忆应可统计");
    assert_eq!(usage.medium(), 0);
    assert_eq!(usage.long(), 1);

    memory
        .clear(PersistentMemoryScope::Long)
        .await
        .expect("长期记忆应可单独清理");
    assert_eq!(
        database
            .agent_memory_usage("persona")
            .await
            .expect("测试记忆应可统计"),
        Default::default()
    );

    for tier in [PersistentMemoryTier::Medium, PersistentMemoryTier::Long] {
        database
            .insert_memory_for_test("persona", tier, true)
            .await
            .expect("测试记忆应可重新写入");
    }
    memory
        .clear(PersistentMemoryScope::All)
        .await
        .expect("全部持久化记忆应可清理");
    assert_eq!(
        database
            .agent_memory_usage("persona")
            .await
            .expect("测试记忆应可统计"),
        Default::default()
    );
    assert_eq!(PersistentMemoryScope::Medium.id(), "medium");
    assert_eq!(PersistentMemoryScope::Long.id(), "long");
    assert_eq!(PersistentMemoryScope::All.id(), "all");
}
