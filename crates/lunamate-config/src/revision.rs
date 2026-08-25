//! 统一分配各配置领域的单调请求 revision，并与配置可见提交临界区排序。

use std::sync::atomic::{AtomicU64, Ordering};

use super::LunaConfig;

impl LunaConfig {
    /// 为帧率写入分配单调 revision。
    pub fn reserve_frame_rate_revision(&self) -> u64 {
        self.reserve_request_revision(&self.frame_rate_request_revision)
    }

    /// 为帧率显示开关写入分配单调 revision。
    pub fn reserve_show_fps_revision(&self) -> u64 {
        self.reserve_request_revision(&self.show_fps_request_revision)
    }

    /// 为原生托盘右键菜单开关写入分配单调 revision。
    pub fn reserve_use_native_tray_menu_revision(&self) -> u64 {
        self.reserve_request_revision(&self.use_native_tray_menu_request_revision)
    }

    /// 为 Agent 换装工具开关写入分配单调 revision。
    pub fn reserve_allow_agent_outfit_change_revision(&self) -> u64 {
        self.reserve_request_revision(&self.allow_agent_outfit_change_request_revision)
    }

    /// 为 Agent 截屏授权写入分配单调 revision。
    pub fn reserve_allow_agent_screenshot_revision(&self, allowed: bool) -> u64 {
        // 撤权必须与截图任务启动互斥；返回后不得再按旧 revision 派发平台捕获。
        let _execution_guard = self.agent_screenshot_execution_gate.write();
        let _guard = self.revision_lock.lock();
        let revision = reserve_revision(&self.allow_agent_screenshot_request_revision);
        self.agent_screenshot_permission_revision_sender
            .send_replace(revision);
        self.requested_allow_agent_screenshot
            .store(allowed, Ordering::Release);
        self.agent_screenshot_permission_retry_required
            .store(false, Ordering::Release);
        revision
    }

    /// 为完整日志配置写入分配单调 revision。
    pub fn reserve_logging_settings_revision(&self) -> u64 {
        self.reserve_request_revision(&self.logging_request_revision)
    }

    /// 为桌宠主窗口尺寸写入分配单调 revision。
    pub fn reserve_model_window_size_revision(&self) -> u64 {
        self.reserve_request_revision(&self.model_window_size_request_revision)
    }

    /// 为眼部跟随开关写入分配单调 revision。
    pub fn reserve_eye_tracking_revision(&self) -> u64 {
        self.reserve_request_revision(&self.eye_tracking_request_revision)
    }

    /// 为外观配置写入分配单调 revision。
    pub fn reserve_appearance_revision(&self) -> u64 {
        self.reserve_request_revision(&self.appearance_request_revision)
    }

    /// 为窗口位置记忆开关写入分配单调 revision。
    pub fn reserve_remember_positions_revision(&self) -> u64 {
        self.reserve_request_revision(&self.remember_positions_request_revision)
    }

    /// 为模型选择写入分配单调 revision。
    pub fn reserve_model_revision(&self) -> u64 {
        self.reserve_request_revision(&self.model_request_revision)
    }

    /// 为完整模型资源覆盖写入分配单调 revision。
    pub fn reserve_model_resource_settings_revision(&self) -> u64 {
        self.reserve_request_revision(&self.model_resources_request_revision)
    }

    /// 为一份由 Agent 设置编辑器提交的草稿分配单调 revision。
    pub fn reserve_llm_settings_revision(&self) -> u64 {
        self.reserve_request_revision(&self.llm_request_revision)
    }

    /// 为一份由人格设置编辑器提交的草稿分配单调 revision。
    pub fn reserve_persona_settings_revision(&self) -> u64 {
        self.reserve_request_revision(&self.persona_request_revision)
    }

    /// 为一份完整快捷键配置分配单调 revision。
    pub fn reserve_shortcut_settings_revision(&self) -> u64 {
        self.reserve_request_revision(&self.shortcut_request_revision)
    }

    /// 为一份完整语音配置分配单调 revision。
    pub fn reserve_voice_settings_revision(&self) -> u64 {
        self.reserve_request_revision(&self.voice_request_revision)
    }

    /// 为窗口位置重置写入分配单调 revision。
    pub fn reserve_reset_positions_revision(&self) -> u64 {
        self.reserve_request_revision(&self.reset_positions_request_revision)
    }

    fn reserve_request_revision(&self, counter: &AtomicU64) -> u64 {
        // 该锁只与最终复核、rename 和内存发布互斥；父目录耐久性同步不得进入此范围。
        let _guard = self.revision_lock.lock();
        reserve_revision(counter)
    }
}

pub fn reserve_revision(counter: &AtomicU64) -> u64 {
    counter
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1)
        .max(1)
}

pub fn revision_is_current(counter: &AtomicU64, revision: u64) -> bool {
    counter.load(Ordering::Relaxed) == revision
}
