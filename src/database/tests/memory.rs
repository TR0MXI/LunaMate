//! 在真实内存后端上验证 `agent_memory` 的按人格统计与删除。

use super::run_async;
use crate::database::{Database, DatabaseError, MemoryTier, MemoryUsage};

#[test]
fn usage_counts_only_valid_entries_of_the_requested_agent() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");
        for tier in [MemoryTier::Medium, MemoryTier::Medium, MemoryTier::Long] {
            database
                .insert_memory_for_test("moon", tier, true)
                .await
                .expect("测试记忆应可写入");
        }
        // 已失效的条目不再参与召回，也不应计入用户看到的用量。
        database
            .insert_memory_for_test("moon", MemoryTier::Long, false)
            .await
            .expect("失效记忆应可写入");
        database
            .insert_memory_for_test("other", MemoryTier::Medium, true)
            .await
            .expect("其他人格的记忆应可写入");

        assert_eq!(
            database
                .agent_memory_usage("moon")
                .await
                .expect("统计应成功"),
            MemoryUsage { medium: 2, long: 1 }
        );
        assert_eq!(
            database
                .agent_memory_usage("other")
                .await
                .expect("统计应成功"),
            MemoryUsage { medium: 1, long: 0 }
        );
    });
}

#[test]
fn an_agent_without_memory_reports_zero_instead_of_failing() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");

        assert_eq!(
            database
                .agent_memory_usage("fresh")
                .await
                .expect("空表统计应成功"),
            MemoryUsage::default()
        );
    });
}

#[test]
fn clearing_one_tier_keeps_the_other_tier_and_other_agents() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");
        for agent in ["moon", "other"] {
            for tier in [MemoryTier::Medium, MemoryTier::Long] {
                database
                    .insert_memory_for_test(agent, tier, true)
                    .await
                    .expect("测试记忆应可写入");
            }
        }

        database
            .delete_agent_memories("moon", Some(MemoryTier::Medium))
            .await
            .expect("删除中期记忆应成功");

        assert_eq!(
            database
                .agent_memory_usage("moon")
                .await
                .expect("统计应成功"),
            MemoryUsage { medium: 0, long: 1 }
        );
        // 记忆按人格隔离，清除一个人格的一层不得波及其他人格。
        assert_eq!(
            database
                .agent_memory_usage("other")
                .await
                .expect("统计应成功"),
            MemoryUsage { medium: 1, long: 1 }
        );
    });
}

#[test]
fn clearing_without_a_tier_removes_every_memory_of_that_agent_only() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");
        for agent in ["moon", "other"] {
            for tier in [MemoryTier::Medium, MemoryTier::Long] {
                database
                    .insert_memory_for_test(agent, tier, true)
                    .await
                    .expect("测试记忆应可写入");
            }
        }
        database
            .insert_memory_for_test("moon", MemoryTier::Long, false)
            .await
            .expect("失效记忆应可写入");

        database
            .delete_agent_memories("moon", None)
            .await
            .expect("删除全部记忆应成功");

        assert_eq!(
            database
                .agent_memory_usage("moon")
                .await
                .expect("统计应成功"),
            MemoryUsage::default()
        );
        assert_eq!(
            database
                .agent_memory_usage("other")
                .await
                .expect("统计应成功"),
            MemoryUsage { medium: 1, long: 1 }
        );

        // 删除不存在的记录必须是无害的空操作，重复点击清除不应报错。
        database
            .delete_agent_memories("moon", None)
            .await
            .expect("重复删除应成功");
    });
}

#[test]
fn agent_identifiers_are_validated_before_touching_the_database() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");

        for agent in ["", "moon persona", "moon/../other", &"m".repeat(65)] {
            assert!(
                matches!(
                    database.agent_memory_usage(agent).await,
                    Err(DatabaseError::InvalidDocumentKey(_))
                ),
                "{agent:?} 应被拒绝"
            );
            assert!(matches!(
                database.delete_agent_memories(agent, None).await,
                Err(DatabaseError::InvalidDocumentKey(_))
            ));
        }
    });
}

#[test]
fn deleting_a_document_is_idempotent_and_scoped_to_its_key() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");
        database
            .write_document("agent", "chat-session/moon", 1, b"moon")
            .await
            .expect("写入应成功");
        database
            .write_document("agent", "chat-session/other", 1, b"other")
            .await
            .expect("写入应成功");

        database
            .delete_document("agent", "chat-session/moon")
            .await
            .expect("删除应成功");
        // 文档不存在时删除同样视为成功，避免清除按钮在空会话上报错。
        database
            .delete_document("agent", "chat-session/moon")
            .await
            .expect("重复删除应成功");

        assert!(
            database
                .read_document("agent", "chat-session/moon")
                .await
                .expect("读取应成功")
                .is_none()
        );
        assert!(
            database
                .read_document("agent", "chat-session/other")
                .await
                .expect("读取应成功")
                .is_some()
        );
    });
}
