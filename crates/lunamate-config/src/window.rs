//! 管理窗口尺寸、位置缓存与位置持久化。

use std::sync::atomic::Ordering;

use toml_edit::Value;

use super::{
    ConfigWindow, ConfigWriteError, LunaConfig, ModelWindowSize, WindowPosition, WindowPositions,
    ensure_table_like, remove_key, set_item_value, write_window_position,
};

impl LunaConfig {
    /// 仅提交仍然最新的桌宠主窗口尺寸写入。
    pub fn set_model_window_size_at_revision(
        &self,
        size: ModelWindowSize,
        revision: u64,
    ) -> Result<Option<()>, ConfigWriteError> {
        let applied = self.edit_config_at_revision(
            &self.model_window_size_request_revision,
            revision,
            "window.model_size",
            || {
                self.model_window_size
                    .store(size.atomic_value(), Ordering::Relaxed);
            },
            |document| {
                ensure_table_like(&mut document["window"]);
                set_item_value(
                    &mut document["window"]["model_size"],
                    Value::from(size.id()),
                );
            },
        )?;
        Ok(applied.then_some(()))
    }

    /// 仅提交仍然最新的窗口位置记忆开关写入。
    pub fn set_remember_window_positions_at_revision(
        &self,
        remember: bool,
        revision: u64,
    ) -> Result<Option<()>, ConfigWriteError> {
        let _position_guard = self.window_position_write_lock.lock();
        let applied = self.edit_config_at_revision(
            &self.remember_positions_request_revision,
            revision,
            "window.remember_position",
            || {
                self.remember_window_positions
                    .store(remember, Ordering::Relaxed);
            },
            |document| {
                ensure_table_like(&mut document["window"]);
                set_item_value(
                    &mut document["window"]["remember_position"],
                    Value::from(remember),
                );
            },
        )?;
        Ok(applied.then_some(()))
    }

    /// 只更新内存中的窗口位置快照；拖动期间不会访问磁盘。
    pub fn cache_window_position(&self, window: ConfigWindow, position: WindowPosition) {
        let mut positions = self.window_positions.lock();
        if positions.window_position(window) != Some(position) {
            positions.set_window_position(window, Some(position));
        }
    }

    /// 将已缓存的两个窗口位置集中写入配置文件。
    ///
    /// 关闭位置保存时该方法不访问磁盘。
    ///
    /// # Errors
    ///
    /// 配置目录或文件无法读取、创建或写入时返回错误。
    pub fn persist_window_positions(&self) -> Result<(), ConfigWriteError> {
        let _position_guard = self.window_position_write_lock.lock();
        if !self.remember_window_positions() {
            return Ok(());
        }
        let positions = *self.window_positions.lock();
        self.edit_document(|document| {
            write_window_position(document, ConfigWindow::DesktopPet, positions.desktop_pet);
            write_window_position(document, ConfigWindow::Settings, positions.settings);
        })
    }

    /// 仅提交仍然最新的窗口位置重置。
    pub fn reset_window_positions_at_revision(
        &self,
        revision: u64,
    ) -> Result<Option<()>, ConfigWriteError> {
        let _position_guard = self.window_position_write_lock.lock();
        let applied = self.edit_config_at_revision(
            &self.reset_positions_request_revision,
            revision,
            "window.saved_positions",
            || {
                *self.window_positions.lock() = WindowPositions::default();
            },
            |document| {
                remove_key(document, "window", ConfigWindow::DesktopPet.table_name());
                remove_key(document, "window", ConfigWindow::Settings.table_name());
            },
        )?;
        Ok(applied.then_some(()))
    }
}
