//! 按 Agent 标识统计与清除 `agent_memory` 记录，不向调用方暴露 SurrealDB 类型。
//!
//! 中期与长期记忆的写入与召回尚未实现；本模块目前只提供人格设置界面需要的
//! 用量统计和删除入口，使记忆生命周期从一开始就可被用户完全掌控。

use super::{
    DatabaseError,
    engine::{Database, validate_agent_id},
};

const MEMORY_TABLE: &str = "agent_memory";

/// `agent_memory.tier` 的两个持久化取值。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MemoryTier {
    /// 由后台模型从对话中提取的条目化记忆。
    Medium,
    /// 供 RAG 检索的长期记忆。
    Long,
}

impl MemoryTier {
    /// 返回写入数据库的稳定标识。
    pub(crate) const fn id(self) -> &'static str {
        match self {
            Self::Medium => "medium",
            Self::Long => "long",
        }
    }
}

/// 单个 Agent 当前占用的中期与长期记忆条数。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct MemoryUsage {
    pub(crate) medium: u64,
    pub(crate) long: u64,
}

impl Database {
    /// 统计指定 Agent 的有效中期与长期记忆条数。
    ///
    /// # Errors
    ///
    /// Agent 标识不合法或数据库查询失败时返回错误。
    pub(crate) async fn agent_memory_usage(
        &self,
        agent_id: &str,
    ) -> Result<MemoryUsage, DatabaseError> {
        validate_agent_id(agent_id)?;
        Ok(MemoryUsage {
            medium: self.count_tier(agent_id, MemoryTier::Medium).await?,
            long: self.count_tier(agent_id, MemoryTier::Long).await?,
        })
    }

    async fn count_tier(&self, agent_id: &str, tier: MemoryTier) -> Result<u64, DatabaseError> {
        let mut response = self
            .client
            .query(format!(
                "SELECT count() FROM {MEMORY_TABLE} \
                 WHERE agent_id = $agent_id AND valid = true AND tier = $tier GROUP ALL;"
            ))
            .bind(("agent_id", agent_id.to_owned()))
            .bind(("tier", tier.id()))
            .await
            .and_then(|response| response.check())
            .map_err(|source| DatabaseError::Engine {
                operation: "统计 Agent 记忆",
                source: Box::new(source),
            })?;
        // 没有匹配记录时 `GROUP ALL` 返回空结果集，这里等价于零条。
        let count: Option<i64> =
            response
                .take((0, "count"))
                .map_err(|source| DatabaseError::Engine {
                    operation: "解析 Agent 记忆统计",
                    source: Box::new(source),
                })?;
        Ok(count
            .and_then(|count| u64::try_from(count).ok())
            .unwrap_or(0))
    }

    /// 写入一条测试记忆记录，使统计与删除可以在真实引擎上验证。
    ///
    /// # Errors
    ///
    /// Agent 标识不合法或数据库提交失败时返回错误。
    #[cfg(test)]
    pub(crate) async fn insert_memory_for_test(
        &self,
        agent_id: &str,
        tier: MemoryTier,
        valid: bool,
    ) -> Result<(), DatabaseError> {
        validate_agent_id(agent_id)?;
        self.client
            .query(format!(
                "CREATE {MEMORY_TABLE} SET agent_id = $agent_id, tier = $tier, \
                 kind = 'episodic', content = $content, valid = $valid;"
            ))
            .bind(("agent_id", agent_id.to_owned()))
            .bind(("tier", tier.id()))
            .bind(("content", format!("{agent_id}/{}", tier.id())))
            .bind(("valid", valid))
            .await
            .and_then(|response| response.check())
            .map(|_| ())
            .map_err(|source| DatabaseError::Engine {
                operation: "写入测试记忆",
                source: Box::new(source),
            })
    }

    /// 删除指定 Agent 的记忆；`tier` 为空时删除全部层级。
    ///
    /// # Errors
    ///
    /// Agent 标识不合法或数据库提交失败时返回错误。
    pub(crate) async fn delete_agent_memories(
        &self,
        agent_id: &str,
        tier: Option<MemoryTier>,
    ) -> Result<(), DatabaseError> {
        validate_agent_id(agent_id)?;
        let statement = match tier {
            Some(_) => {
                format!("DELETE {MEMORY_TABLE} WHERE agent_id = $agent_id AND tier = $tier;")
            }
            None => format!("DELETE {MEMORY_TABLE} WHERE agent_id = $agent_id;"),
        };
        let mut query = self
            .client
            .query(statement)
            .bind(("agent_id", agent_id.to_owned()));
        if let Some(tier) = tier {
            query = query.bind(("tier", tier.id()));
        }
        query
            .await
            .and_then(|response| response.check())
            .map(|_| ())
            .map_err(|source| DatabaseError::Engine {
                operation: "删除 Agent 记忆",
                source: Box::new(source),
            })
    }
}
