//! 验证文档键校验规则、隔离性与数据库错误的脱敏展示。

use std::error::Error as _;

use super::run_async;
use crate::database::{Database, DatabaseError, engine::MAX_DOCUMENT_BYTES};

#[test]
fn missing_document_reads_as_none() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");

        let document = database
            .read_document("agent", "absent")
            .await
            .expect("读取缺失文档不应报错");

        assert!(document.is_none());
    });
}

#[test]
fn documents_are_isolated_by_scope_and_key() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");

        database
            .write_document("agent", "session", 1, b"agent-session")
            .await
            .expect("写入首个文档应成功");
        database
            .write_document("agent", "other", 1, b"agent-other")
            .await
            .expect("写入同作用域的第二个文档应成功");
        database
            .write_document("ui", "session", 1, b"ui-session")
            .await
            .expect("写入另一作用域的文档应成功");

        for (scope, key, expected) in [
            ("agent", "session", b"agent-session".as_slice()),
            ("agent", "other", b"agent-other".as_slice()),
            ("ui", "session", b"ui-session".as_slice()),
        ] {
            let document = database
                .read_document(scope, key)
                .await
                .expect("读取已写入文档应成功")
                .expect("已写入文档应当存在");
            assert_eq!(document.contents(), expected);
        }
    });
}

#[test]
fn zero_format_version_is_rejected_before_querying() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");

        assert!(matches!(
            database
                .write_document("agent", "session", 0, b"value")
                .await,
            Err(DatabaseError::InvalidDocumentVersion)
        ));
    });
}

#[test]
fn scope_must_be_bounded_lowercase_ascii() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");

        for scope in ["Agent", "agent scope", "代理", &"a".repeat(65)] {
            assert!(
                matches!(
                    database.write_document(scope, "session", 1, b"value").await,
                    Err(DatabaseError::InvalidDocumentKey(_))
                ),
                "作用域 {scope:?} 应当被拒绝"
            );
        }

        for scope in ["agent", "agent-2", "agent_2", &"a".repeat(64)] {
            assert!(
                database
                    .write_document(scope, "session", 1, b"value")
                    .await
                    .is_ok(),
                "作用域 {scope:?} 应当被接受"
            );
        }
    });
}

#[test]
fn key_rejects_control_characters_and_oversized_input() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");

        for key in ["line\nbreak", "null\0byte", &"k".repeat(257)] {
            assert!(
                matches!(
                    database.write_document("agent", key, 1, b"value").await,
                    Err(DatabaseError::InvalidDocumentKey(_))
                ),
                "键 {key:?} 应当被拒绝"
            );
        }

        // 非 ASCII 但可打印的键仍在 UTF-8 字节上限内，应当可用。
        database
            .write_document("agent", "会话 1", 1, b"value")
            .await
            .expect("可打印 Unicode 键应当被接受");
        assert!(
            database
                .read_document("agent", "会话 1")
                .await
                .expect("读取应成功")
                .is_some()
        );
    });
}

#[test]
fn document_at_the_size_limit_round_trips() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");
        let contents = vec![0xA5; MAX_DOCUMENT_BYTES];

        database
            .write_document("agent", "session", 3, &contents)
            .await
            .expect("恰好达到上限的文档应当可以写入");
        let document = database
            .read_document("agent", "session")
            .await
            .expect("读取应成功")
            .expect("文档应当存在");

        assert_eq!(document.format_version(), 3);
        assert_eq!(document.contents().len(), MAX_DOCUMENT_BYTES);
    });
}

#[test]
fn database_errors_describe_context_without_leaking_contents() {
    let too_large = DatabaseError::DocumentTooLarge {
        actual: 16,
        maximum: 8,
    };
    let message = too_large.to_string();
    assert!(message.contains("16"));
    assert!(message.contains('8'));
    assert!(too_large.source().is_none());

    assert!(
        DatabaseError::InvalidDocumentKey("作用域必须为 1 到 64 个 ASCII 字节")
            .to_string()
            .contains("作用域")
    );
    assert!(
        DatabaseError::InvalidDocumentVersion
            .to_string()
            .contains("版本")
    );
    assert!(
        DatabaseError::InvalidStoredDocument
            .to_string()
            .contains("结构不完整")
    );

    let create_directory = DatabaseError::CreateDirectory {
        path: std::path::PathBuf::from("/tmp/lunamate-missing"),
        source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
    };
    assert!(create_directory.to_string().contains("lunamate-missing"));
    assert!(create_directory.source().is_some());
}
