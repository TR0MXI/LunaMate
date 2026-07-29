//! 管理嵌入式数据库、持久化模式与配置文件原子替换，并隐藏具体存储引擎。
//!
//! 生产环境只连接 SurrealKV；内存引擎仅在本模块测试中用于隔离测试数据。

mod atomic_file;
mod engine;
mod memory;
mod schema;

#[cfg(test)]
mod tests;

use std::{error::Error, fmt, io, path::PathBuf};

pub(crate) use atomic_file::{AtomicReplaceOperation, atomic_replace};
pub(crate) use engine::{Database, StoredDocument};
pub(crate) use memory::{MemoryTier, MemoryUsage};

/// 描述嵌入式数据库初始化或文档访问失败。
#[derive(Debug)]
pub(crate) enum DatabaseError {
    CreateDirectory {
        path: PathBuf,
        source: io::Error,
    },
    #[cfg(unix)]
    SetDirectoryPermissions {
        path: PathBuf,
        source: io::Error,
    },
    Open {
        path: PathBuf,
        source: Box<surrealdb::Error>,
    },
    Engine {
        operation: &'static str,
        source: Box<surrealdb::Error>,
    },
    InvalidDocumentKey(&'static str),
    InvalidDocumentVersion,
    DocumentTooLarge {
        actual: usize,
        maximum: usize,
    },
    InvalidStoredDocument,
}

impl DatabaseError {
    /// 返回不含路径、查询或文档内容的稳定诊断分类。
    pub(crate) const fn diagnostic_kind(&self) -> &'static str {
        match self {
            Self::CreateDirectory { .. } => "create_directory",
            #[cfg(unix)]
            Self::SetDirectoryPermissions { .. } => "set_directory_permissions",
            Self::Open { .. } => "open",
            Self::Engine { .. } => "engine",
            Self::InvalidDocumentKey(_) => "invalid_document_key",
            Self::InvalidDocumentVersion => "invalid_document_version",
            Self::DocumentTooLarge { .. } => "document_too_large",
            Self::InvalidStoredDocument => "invalid_stored_document",
        }
    }
}

impl fmt::Display for DatabaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateDirectory { path, source } => {
                write!(
                    formatter,
                    "创建数据库目录 {} 失败：{source}",
                    path.display()
                )
            }
            #[cfg(unix)]
            Self::SetDirectoryPermissions { path, source } => write!(
                formatter,
                "限制数据库目录 {} 的访问权限失败：{source}",
                path.display()
            ),
            Self::Open { path, source } => {
                write!(
                    formatter,
                    "打开嵌入式数据库 {} 失败：{source}",
                    path.display()
                )
            }
            Self::Engine { operation, source } => {
                write!(formatter, "{operation}失败：{source}")
            }
            Self::InvalidDocumentKey(reason) => write!(formatter, "存储文档键无效：{reason}"),
            Self::InvalidDocumentVersion => write!(formatter, "存储文档版本必须大于零"),
            Self::DocumentTooLarge { actual, maximum } => write!(
                formatter,
                "存储文档包含 {actual} 字节，超过 {maximum} 字节上限"
            ),
            Self::InvalidStoredDocument => write!(formatter, "数据库中的存储文档结构不完整"),
        }
    }
}

impl Error for DatabaseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CreateDirectory { source, .. } => Some(source),
            #[cfg(unix)]
            Self::SetDirectoryPermissions { source, .. } => Some(source),
            Self::Open { source, .. } | Self::Engine { source, .. } => Some(source),
            Self::InvalidDocumentKey(_)
            | Self::InvalidDocumentVersion
            | Self::DocumentTooLarge { .. }
            | Self::InvalidStoredDocument => None,
        }
    }
}
