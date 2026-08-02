//! 验证配置原子替换的临时文件隔离、权限收紧与失败清理。

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::database::{AtomicReplaceOperation, atomic_replace, prepare_atomic_replace};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间必须晚于 Unix 纪元")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "lunamate-atomic-file-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("测试目录应当可以创建");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn visible_entries(directory: &Path) -> Vec<String> {
    let mut names = fs::read_dir(directory)
        .expect("测试目录应当可以枚举")
        .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned()))
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn replace_creates_target_and_leaves_no_temporary_file() {
    let directory = TestDirectory::new();
    let target = directory.path().join("config.toml");

    atomic_replace(&target, b"first = 1\n", 7).expect("首次原子写入应当成功");

    assert_eq!(
        fs::read(&target).expect("目标文件应当可以读取"),
        b"first = 1\n"
    );
    assert_eq!(visible_entries(directory.path()), vec!["config.toml"]);
}

#[test]
fn replace_overwrites_existing_contents_atomically() {
    let directory = TestDirectory::new();
    let target = directory.path().join("config.toml");
    fs::write(&target, b"stale contents that is much longer").expect("旧文件应当可以创建");

    atomic_replace(&target, b"new = true\n", 1).expect("覆盖写入应当成功");
    atomic_replace(&target, b"newer = true\n", 2).expect("再次覆盖写入应当成功");

    assert_eq!(
        fs::read(&target).expect("目标文件应当可以读取"),
        b"newer = true\n"
    );
    assert_eq!(visible_entries(directory.path()), vec!["config.toml"]);
}

#[test]
fn prepared_replace_keeps_target_unchanged_until_replace() {
    let directory = TestDirectory::new();
    let target = directory.path().join("config.toml");
    fs::write(&target, b"published = true\n").expect("旧文件应当可以创建");

    let prepared =
        prepare_atomic_replace(&target, b"draft = true\n", 17).expect("待提交临时文件应当可以准备");

    assert_eq!(
        fs::read(&target).expect("prepare 后目标仍应可读"),
        b"published = true\n"
    );
    assert_eq!(visible_entries(directory.path()).len(), 2);
    let temporary_path = fs::read_dir(directory.path())
        .expect("测试目录应当可以枚举")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path != &target)
        .expect("prepare 应当留下一个待提交临时文件");
    assert_eq!(
        fs::read(&temporary_path).expect("待提交临时文件应当可以读取"),
        b"draft = true\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let mode = fs::metadata(&temporary_path)
            .expect("待提交临时文件元数据应当可读")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    let visible = prepared.replace().expect("准备完成的替换应当可以变为可见");

    assert_eq!(
        fs::read(&target).expect("可见提交后目标应当可读"),
        b"draft = true\n"
    );
    assert_eq!(visible_entries(directory.path()), vec!["config.toml"]);

    visible.sync_parent().expect("父目录耐久性同步应当成功");
}

#[test]
fn dropping_prepared_replace_removes_temporary_file_without_changing_target() {
    let directory = TestDirectory::new();
    let target = directory.path().join("config.toml");
    fs::write(&target, b"published = true\n").expect("旧文件应当可以创建");

    let prepared = prepare_atomic_replace(&target, b"abandoned = true\n", 19)
        .expect("待丢弃临时文件应当可以准备");
    drop(prepared);

    assert_eq!(
        fs::read(&target).expect("丢弃 prepare 后目标仍应可读"),
        b"published = true\n"
    );
    assert_eq!(visible_entries(directory.path()), vec!["config.toml"]);
}

#[test]
fn empty_contents_truncate_the_previous_document() {
    let directory = TestDirectory::new();
    let target = directory.path().join("config.toml");
    fs::write(&target, b"previous").expect("旧文件应当可以创建");

    atomic_replace(&target, b"", 3).expect("空内容写入应当成功");

    assert!(fs::read(&target).expect("目标文件应当可以读取").is_empty());
}

#[test]
fn missing_parent_directory_reports_temporary_creation_failure() {
    let directory = TestDirectory::new();
    let target = directory.path().join("missing/config.toml");

    let error = atomic_replace(&target, b"value = 1\n", 5).expect_err("缺失父目录应当返回错误");
    let (operation, path, source) = error.into_parts();

    assert!(matches!(operation, AtomicReplaceOperation::CreateTemporary));
    assert_eq!(path.parent(), target.parent());
    assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn failed_replace_does_not_leave_a_temporary_file_behind() {
    let directory = TestDirectory::new();
    // 目标是目录时重命名必定失败，用于验证失败路径会清理临时文件。
    let target = directory.path().join("config.toml");
    fs::create_dir(&target).expect("占位目录应当可以创建");

    let error = atomic_replace(&target, b"value = 1\n", 9).expect_err("目标为目录时应当失败");
    let (operation, _, _) = error.into_parts();

    assert!(matches!(operation, AtomicReplaceOperation::Replace));
    assert_eq!(visible_entries(directory.path()), vec!["config.toml"]);
}

#[cfg(unix)]
#[test]
fn parent_sync_failure_is_reported_after_replacement_becomes_visible() {
    let directory = TestDirectory::new();
    let target = directory.path().join("config.toml");
    let mut prepared = prepare_atomic_replace(&target, b"visible = true\n", 23)
        .expect("待提交临时文件应当可以准备");
    prepared.fail_parent_sync_for_test();

    let visible = prepared.replace().expect("rename 可见提交应当成功");
    assert_eq!(
        fs::read(&target).expect("父目录同步前新目标应当已经可见"),
        b"visible = true\n"
    );

    let error = visible
        .sync_parent()
        .expect_err("测试注入应当使父目录同步失败");
    let (operation, path, source) = error.into_parts();
    assert!(matches!(operation, AtomicReplaceOperation::SyncParent));
    assert_eq!(path, directory.path());
    assert_eq!(source.kind(), std::io::ErrorKind::Other);
    assert_eq!(visible_entries(directory.path()), vec!["config.toml"]);
}

#[test]
fn concurrent_callers_do_not_reuse_one_temporary_path() {
    let directory = TestDirectory::new();
    let first = directory.path().join("first.toml");
    let second = directory.path().join("second.toml");

    std::thread::scope(|scope| {
        for (target, contents) in [(&first, b"first"), (&second, b"secon")] {
            scope.spawn(move || {
                for nonce in 0..16 {
                    atomic_replace(target, contents, nonce).expect("并发原子写入应当成功");
                }
            });
        }
    });

    assert_eq!(fs::read(&first).expect("首个目标应当可以读取"), b"first");
    assert_eq!(fs::read(&second).expect("次个目标应当可以读取"), b"secon");
    assert_eq!(
        visible_entries(directory.path()),
        vec!["first.toml", "second.toml"]
    );
}

#[cfg(unix)]
#[test]
fn replaced_file_is_only_accessible_by_the_current_user() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = TestDirectory::new();
    let target = directory.path().join("config.toml");

    atomic_replace(&target, b"secret = \"token\"\n", 13).expect("原子写入应当成功");

    let mode = fs::metadata(&target)
        .expect("目标文件元数据应当可读")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}
