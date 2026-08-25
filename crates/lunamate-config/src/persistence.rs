//! 协调配置文档编辑、两阶段原子替换与 revision 发布。

use std::{fs, path::Path, sync::atomic::AtomicU64};

#[cfg(test)]
use std::sync::atomic::Ordering;
use toml_edit::DocumentMut;

use crate::config::atomic_file::PreparedAtomicReplace;

use super::{
    ConfigWriteError, LunaConfig,
    document::{
        document_for_update, prepare_config_file, replace_config_file, sync_config_file_parent,
        write_config_file,
    },
    revision::{reserve_revision, revision_is_current},
};

impl LunaConfig {
    pub fn edit_document(
        &self,
        edit: impl FnOnce(&mut DocumentMut),
    ) -> Result<(), ConfigWriteError> {
        let _guard = self.write_lock.lock();
        self.edit_document_locked(edit)
    }

    fn edit_document_locked(
        &self,
        edit: impl FnOnce(&mut DocumentMut),
    ) -> Result<(), ConfigWriteError> {
        let (path, document, nonce) = self.edited_document_locked(edit)?;
        // 无 revision 的窗口位置等普通写入继续走完整 atomic_replace。
        write_config_file(path, &document, nonce)
    }

    pub fn prepare_document_locked(
        &self,
        edit: impl FnOnce(&mut DocumentMut),
    ) -> Result<PreparedAtomicReplace, ConfigWriteError> {
        let (path, document, nonce) = self.edited_document_locked(edit)?;
        #[cfg(test)]
        if self.prepare_failure_for_test.swap(false, Ordering::AcqRel) {
            return Err(ConfigWriteError::Io {
                operation: "测试模拟配置 prepare 失败",
                path: path.to_path_buf(),
                source: std::io::Error::other("测试注入的 prepare 失败"),
            });
        }
        let prepared = prepare_config_file(path, &document, nonce)?;
        #[cfg(test)]
        let mut prepared = prepared;
        #[cfg(test)]
        if let Some(barrier) = self.parent_sync_barrier_for_test.lock().take() {
            prepared.block_parent_sync_for_test(barrier);
        }
        #[cfg(all(test, unix))]
        if self
            .parent_sync_failure_for_test
            .swap(false, Ordering::AcqRel)
        {
            prepared.fail_parent_sync_for_test();
        }
        Ok(prepared)
    }

    fn edited_document_locked(
        &self,
        edit: impl FnOnce(&mut DocumentMut),
    ) -> Result<(&Path, DocumentMut, u64), ConfigWriteError> {
        let path = self
            .path
            .as_deref()
            .ok_or(ConfigWriteError::PersistenceUnavailable)?;
        let nonce = reserve_revision(&self.write_nonce);
        let mut document = document_for_update(path, nonce)?;
        edit(&mut document);

        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|source| ConfigWriteError::Io {
                operation: "创建配置目录",
                path: parent.to_path_buf(),
                source,
            })?;
        }
        Ok((path, document, nonce))
    }

    pub fn commit_prepared_config_at_revision<T>(
        &self,
        counter: &AtomicU64,
        revision: u64,
        prepared: PreparedAtomicReplace,
        publish: impl FnOnce() -> T,
    ) -> Result<Option<T>, ConfigWriteError> {
        #[cfg(test)]
        self.pause_after_prepare_for_test();
        let (visible, published) = {
            let _revision_guard = self.revision_lock.lock();
            if !revision_is_current(counter, revision) {
                return Ok(None);
            }
            // 最终复核、rename 可见点与内存发布不可被新的 reservation 插入。
            let visible = replace_config_file(prepared)?;
            let published = publish();
            (visible, published)
        };
        sync_config_file_parent(visible)?;
        Ok(Some(published))
    }

    pub fn edit_config_at_revision(
        &self,
        counter: &AtomicU64,
        revision: u64,
        setting: &'static str,
        publish: impl FnOnce(),
        edit: impl FnOnce(&mut DocumentMut),
    ) -> Result<bool, ConfigWriteError> {
        let result = (|| {
            let _guard = self.write_lock.lock();
            if !revision_is_current(counter, revision) {
                return Ok(false);
            }
            let prepared = self.prepare_document_locked(edit)?;
            Ok(self
                .commit_prepared_config_at_revision(counter, revision, prepared, publish)?
                .is_some())
        })();
        log_config_update(setting, revision, result.as_ref().map(|applied| *applied));
        result
    }
}

pub fn log_config_update(setting: &str, revision: u64, outcome: Result<bool, &ConfigWriteError>) {
    match outcome {
        Ok(true) => log::info!("event=config_updated setting={setting} revision={revision}"),
        Ok(false) => {
            log::debug!("event=config_update_superseded setting={setting} revision={revision}");
        }
        Err(error) => log::error!(
            "event=config_update_failed setting={setting} revision={revision} error_kind={}",
            error.diagnostic_kind()
        ),
    }
}
