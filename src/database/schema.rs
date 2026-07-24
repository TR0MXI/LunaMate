//! 定义可重复执行的数据库首版模式与记忆检索索引。

use surrealdb::{Surreal, engine::local::Db};

use super::DatabaseError;

/// 向量维度取决于后续选择的 embedding 模型，因此首版只约束向量字段，不创建固定维度索引。
const INITIAL_SCHEMA: &str = r#"
BEGIN TRANSACTION;

DEFINE TABLE IF NOT EXISTS schema_version SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS version ON schema_version TYPE int ASSERT $value > 0;
DEFINE FIELD IF NOT EXISTS updated_at ON schema_version TYPE datetime VALUE time::now();

DEFINE TABLE IF NOT EXISTS app_storage SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS scope ON app_storage TYPE string;
DEFINE FIELD IF NOT EXISTS document_key ON app_storage TYPE string;
DEFINE FIELD IF NOT EXISTS format_version ON app_storage TYPE int ASSERT $value > 0;
DEFINE FIELD IF NOT EXISTS payload ON app_storage TYPE bytes;
DEFINE FIELD IF NOT EXISTS updated_at ON app_storage TYPE datetime VALUE time::now();
DEFINE INDEX IF NOT EXISTS app_storage_identity ON app_storage FIELDS scope, document_key UNIQUE;

DEFINE TABLE IF NOT EXISTS agent_memory SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS agent_id ON agent_memory TYPE string;
DEFINE FIELD IF NOT EXISTS tier ON agent_memory TYPE "medium" | "long";
DEFINE FIELD IF NOT EXISTS kind ON agent_memory TYPE "episodic" | "semantic" | "procedural" | "profile";
DEFINE FIELD IF NOT EXISTS content ON agent_memory TYPE string;
DEFINE FIELD IF NOT EXISTS summary ON agent_memory TYPE option<string>;
DEFINE FIELD IF NOT EXISTS embedding ON agent_memory TYPE option<array<float>>;
DEFINE FIELD IF NOT EXISTS tags ON agent_memory TYPE array<string> DEFAULT [];
DEFINE FIELD IF NOT EXISTS importance ON agent_memory TYPE float DEFAULT 0.5 ASSERT $value >= 0.0 AND $value <= 1.0;
DEFINE FIELD IF NOT EXISTS confidence ON agent_memory TYPE float DEFAULT 1.0 ASSERT $value >= 0.0 AND $value <= 1.0;
DEFINE FIELD IF NOT EXISTS valid ON agent_memory TYPE bool DEFAULT true;
DEFINE FIELD IF NOT EXISTS source ON agent_memory TYPE option<string>;
DEFINE FIELD IF NOT EXISTS metadata ON agent_memory TYPE object FLEXIBLE DEFAULT {};
DEFINE FIELD IF NOT EXISTS created_at ON agent_memory TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD IF NOT EXISTS updated_at ON agent_memory TYPE datetime VALUE time::now();
DEFINE FIELD IF NOT EXISTS last_accessed_at ON agent_memory TYPE datetime DEFAULT time::now();
DEFINE FIELD IF NOT EXISTS expires_at ON agent_memory TYPE option<datetime>;
DEFINE INDEX IF NOT EXISTS agent_memory_active_tier ON agent_memory FIELDS agent_id, valid, tier, updated_at;
DEFINE INDEX IF NOT EXISTS agent_memory_expiry ON agent_memory FIELDS expires_at;
DEFINE INDEX IF NOT EXISTS agent_memory_tags ON agent_memory FIELDS tags.*;
DEFINE ANALYZER IF NOT EXISTS agent_memory_text TOKENIZERS class, punct FILTERS lowercase;
DEFINE INDEX IF NOT EXISTS agent_memory_content ON agent_memory FIELDS content FULLTEXT ANALYZER agent_memory_text BM25;

UPSERT ONLY schema_version:current SET version = 1;

COMMIT TRANSACTION;
"#;

pub(super) async fn initialize(client: &Surreal<Db>) -> Result<(), DatabaseError> {
    client
        .query(INITIAL_SCHEMA)
        .await
        .and_then(|response| response.check())
        .map(|_| ())
        .map_err(|source| DatabaseError::Engine {
            operation: "初始化数据库模式",
            source: Box::new(source),
        })
}
