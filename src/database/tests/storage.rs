use super::run_async;
use crate::database::{Database, DatabaseError, engine::MAX_DOCUMENT_BYTES, schema};

#[test]
fn document_round_trip_and_overwrite() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");

        database
            .write_document("agent", "session", 1, b"first")
            .await
            .expect("首次写入应成功");
        let first = database
            .read_document("agent", "session")
            .await
            .expect("首次读取应成功")
            .expect("首次写入后应存在文档");
        assert_eq!(first.format_version(), 1);
        assert_eq!(first.contents(), b"first");

        database
            .write_document("agent", "session", 2, b"second")
            .await
            .expect("覆盖写入应成功");
        let second = database
            .read_document("agent", "session")
            .await
            .expect("覆盖后读取应成功")
            .expect("覆盖后应存在文档");
        assert_eq!(second.format_version(), 2);
        assert_eq!(second.contents(), b"second");
    });
}

#[test]
fn schema_initialization_is_idempotent_and_memory_table_is_writable() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");
        schema::initialize(&database.client)
            .await
            .expect("重复初始化 schema 应成功");

        let response = database
            .client
            .query(
                "CREATE ONLY agent_memory:test SET agent_id = 'default', tier = 'medium', \
                 kind = 'episodic', content = 'hello';",
            )
            .await
            .expect("记忆记录写入请求应执行")
            .check();
        assert!(response.is_ok(), "agent_memory 应接受符合 schema 的记录");
    });
}

#[test]
fn document_bounds_are_enforced_before_querying() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");

        assert!(matches!(
            database.read_document("", "session").await,
            Err(DatabaseError::InvalidDocumentKey(_))
        ));
        assert!(matches!(
            database.write_document("agent", "", 1, b"value").await,
            Err(DatabaseError::InvalidDocumentKey(_))
        ));

        let oversized = vec![0; MAX_DOCUMENT_BYTES + 1];
        assert!(matches!(
            database
                .write_document("agent", "session", 1, &oversized)
                .await,
            Err(DatabaseError::DocumentTooLarge { .. })
        ));
    });
}
