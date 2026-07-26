//! 连接 SurrealKV，并提供与具体数据库类型解耦的有界文档存储接口。

use std::{fs, path::PathBuf, sync::Arc};

use surrealdb::{
    Surreal,
    engine::local::{Db, SurrealKv},
    opt::{Config, capabilities::Capabilities},
    types::{Bytes, RecordId},
};

#[cfg(test)]
use surrealdb::engine::local::Mem;

use super::{DatabaseError, schema};

const DATABASE_PATH: &str = "./data/lunamate.db";
const NAMESPACE: &str = "lunamate";
const DATABASE_NAME: &str = "main";
const STORAGE_TABLE: &str = "app_storage";
const MAX_SCOPE_BYTES: usize = 64;
const MAX_KEY_BYTES: usize = 256;
const MAX_AGENT_ID_BYTES: usize = 64;
pub(super) const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;

/// 从常规文档存储恢复的版本化二进制内容。
pub(crate) struct StoredDocument {
    format_version: u32,
    contents: Vec<u8>,
}

impl StoredDocument {
    pub(crate) fn format_version(&self) -> u32 {
        self.format_version
    }

    pub(crate) fn contents(&self) -> &[u8] {
        &self.contents
    }
}

/// 持有单个嵌入式数据库连接，并在 façade 内隔离 SurrealDB API。
pub(crate) struct Database {
    pub(super) client: Surreal<Db>,
}

impl Database {
    /// 在工作目录的固定数据路径创建并打开唯一的 SurrealKV 数据库。
    ///
    /// # Errors
    ///
    /// 数据目录无法创建或限制权限、SurrealKV 无法打开、数据库选择或模式初始化失败时返回错误。
    pub(crate) async fn open_default() -> Result<Arc<Self>, DatabaseError> {
        Self::open_surreal_kv(PathBuf::from(DATABASE_PATH)).await
    }

    async fn open_surreal_kv(path: PathBuf) -> Result<Arc<Self>, DatabaseError> {
        prepare_database_directory(&path)?;
        let client = Surreal::new::<SurrealKv>((path.clone(), embedded_config()))
            .await
            .map_err(|source| DatabaseError::Open {
                path,
                source: Box::new(source),
            })?;
        Self::finish_open(client).await
    }

    #[cfg(test)]
    pub(crate) async fn open_memory() -> Result<Arc<Self>, DatabaseError> {
        let client = Surreal::new::<Mem>(embedded_config())
            .await
            .map_err(|source| DatabaseError::Engine {
                operation: "打开测试内存数据库",
                source: Box::new(source),
            })?;
        Self::finish_open(client).await
    }

    async fn finish_open(client: Surreal<Db>) -> Result<Arc<Self>, DatabaseError> {
        client
            .use_ns(NAMESPACE)
            .use_db(DATABASE_NAME)
            .await
            .map_err(|source| DatabaseError::Engine {
                operation: "选择数据库命名空间",
                source: Box::new(source),
            })?;
        schema::initialize(&client).await?;
        Ok(Arc::new(Self { client }))
    }

    /// 按稳定作用域和键读取一份版本化文档。
    ///
    /// # Errors
    ///
    /// 键不合法、查询失败或数据库内容缺少必要字段时返回错误。
    pub(crate) async fn read_document(
        &self,
        scope: &str,
        key: &str,
    ) -> Result<Option<StoredDocument>, DatabaseError> {
        let record = document_record(scope, key)?;
        let mut response = self
            .client
            .query("SELECT payload, format_version FROM ONLY $record;")
            .bind(("record", record))
            .await
            .and_then(|response| response.check())
            .map_err(|source| DatabaseError::Engine {
                operation: "读取存储文档",
                source: Box::new(source),
            })?;
        let payload: Option<Bytes> =
            response
                .take((0, "payload"))
                .map_err(|source| DatabaseError::Engine {
                    operation: "解析存储文档内容",
                    source: Box::new(source),
                })?;
        let format_version: Option<u32> =
            response
                .take((0, "format_version"))
                .map_err(|source| DatabaseError::Engine {
                    operation: "解析存储文档版本",
                    source: Box::new(source),
                })?;

        match (payload, format_version) {
            (None, None) => Ok(None),
            (Some(payload), Some(format_version)) => Ok(Some(StoredDocument {
                format_version,
                contents: payload.into_inner().to_vec(),
            })),
            _ => Err(DatabaseError::InvalidStoredDocument),
        }
    }

    /// 原子插入或替换一份有界版本化文档。
    ///
    /// # Errors
    ///
    /// 键、版本或内容大小不合法，或数据库提交失败时返回错误。
    pub(crate) async fn write_document(
        &self,
        scope: &str,
        key: &str,
        format_version: u32,
        contents: &[u8],
    ) -> Result<(), DatabaseError> {
        if format_version == 0 {
            return Err(DatabaseError::InvalidDocumentVersion);
        }
        if contents.len() > MAX_DOCUMENT_BYTES {
            return Err(DatabaseError::DocumentTooLarge {
                actual: contents.len(),
                maximum: MAX_DOCUMENT_BYTES,
            });
        }
        let record = document_record(scope, key)?;
        self.client
            .query(
                "UPSERT ONLY $record SET scope = $scope, document_key = $key, \
                 format_version = $format_version, payload = $payload RETURN NONE;",
            )
            .bind(("record", record))
            .bind(("scope", scope.to_owned()))
            .bind(("key", key.to_owned()))
            .bind(("format_version", format_version))
            .bind(("payload", Bytes::from(contents.to_vec())))
            .await
            .and_then(|response| response.check())
            .map(|_| ())
            .map_err(|source| DatabaseError::Engine {
                operation: "提交存储文档",
                source: Box::new(source),
            })
    }

    /// 删除一份存储文档；文档不存在时同样视为成功。
    ///
    /// # Errors
    ///
    /// 键不合法或数据库提交失败时返回错误。
    pub(crate) async fn delete_document(
        &self,
        scope: &str,
        key: &str,
    ) -> Result<(), DatabaseError> {
        let record = document_record(scope, key)?;
        self.client
            .query("DELETE $record;")
            .bind(("record", record))
            .await
            .and_then(|response| response.check())
            .map(|_| ())
            .map_err(|source| DatabaseError::Engine {
                operation: "删除存储文档",
                source: Box::new(source),
            })
    }
}

/// 校验绑定到记忆记录上的 Agent 标识；它来自配置且会参与查询过滤。
pub(super) fn validate_agent_id(agent_id: &str) -> Result<(), DatabaseError> {
    if agent_id.is_empty() || agent_id.len() > MAX_AGENT_ID_BYTES {
        return Err(DatabaseError::InvalidDocumentKey(
            "Agent 标识必须为 1 到 64 个 ASCII 字节",
        ));
    }
    if !agent_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(DatabaseError::InvalidDocumentKey(
            "Agent 标识只能包含 ASCII 字母、数字、下划线或连字符",
        ));
    }
    Ok(())
}

fn embedded_config() -> Config {
    let capabilities = Capabilities::default().with_live_query_notifications(false);
    Config::default().capabilities(capabilities)
}

fn document_record(scope: &str, key: &str) -> Result<RecordId, DatabaseError> {
    validate_scope(scope)?;
    validate_key(key)?;
    Ok(RecordId::new(STORAGE_TABLE, format!("{scope}:{key}")))
}

fn validate_scope(scope: &str) -> Result<(), DatabaseError> {
    if scope.is_empty() || scope.len() > MAX_SCOPE_BYTES {
        return Err(DatabaseError::InvalidDocumentKey(
            "作用域必须为 1 到 64 个 ASCII 字节",
        ));
    }
    if !scope.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
    }) {
        return Err(DatabaseError::InvalidDocumentKey(
            "作用域只能包含小写 ASCII 字母、数字、下划线或连字符",
        ));
    }
    Ok(())
}

fn validate_key(key: &str) -> Result<(), DatabaseError> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES {
        return Err(DatabaseError::InvalidDocumentKey(
            "键必须为 1 到 256 个 UTF-8 字节",
        ));
    }
    if key.chars().any(char::is_control) {
        return Err(DatabaseError::InvalidDocumentKey("键不能包含控制字符"));
    }
    Ok(())
}

fn prepare_database_directory(path: &PathBuf) -> Result<(), DatabaseError> {
    // 在 Unix 上由 DirBuilder 直接以 0o700 创建，避免目录先以 umask 权限暴露给同机其他用户。
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;

        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|source| DatabaseError::CreateDirectory {
            path: path.clone(),
            source,
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        // 目录已存在时 recursive create 不会改动权限，仍需收紧既有数据目录。
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
            DatabaseError::SetDirectoryPermissions {
                path: path.clone(),
                source,
            }
        })?;
    }
    Ok(())
}
