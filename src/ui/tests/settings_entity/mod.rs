//! 在无头 GPUI TestAppContext 中验证设置窗口实体的状态流转。
//!
//! 设置界面通过全局 `CONFIG` 写入用户配置文件；失败路径直接向完成处理器注入写错误，
//! 因而可以确定性验证草稿、已发布快照和事件，而不会触碰用户配置或依赖跨 runtime 唤醒。

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gpui::{Entity, TestAppContext, VisualTestContext, prelude::*};
use lunamate_agent::{Agent, AgentMemory, ChatLimits, Client, tools::OutfitOption};

use crate::{
    config::{
        AppLanguage, CONFIG, ConfigWriteError, LogLevel, ModelExpressionCategory, ModelResourceKey,
        ModelResourceKind, ModelWindowSize, ThemePreset,
    },
    logging::ApplyLoggingSettingsOutcome,
    model::{ModelCatalog, ModelPreviewCapabilities, ModelPreviewExpression, ModelPreviewResource},
    ui::settings::{AgentOutfitAction, SettingsEventKindForTest, SettingsView, SettingsWindowView},
};

mod lifecycle;
mod model;
mod persistence;
mod render;
mod scanning;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间必须晚于 Unix 纪元")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "lunamate-settings-entity-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("测试模型目录应当可以创建");
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

fn unavailable_agent() -> Arc<Agent> {
    Agent::new(
        Client::default(),
        None,
        None,
        "",
        AgentMemory::unavailable(),
        "default",
        ChatLimits::default(),
        AppLanguage::default(),
        None,
    )
}

/// 后台扫描运行在 GPUI executor 上，这里在有限时间内驱动到稳定状态。
///
/// 上限只用于避免测试永久挂起；正常情况下几毫秒即可收敛，因此取值足够宽松，
/// 使并行测试与覆盖率插桩带来的调度抖动不会造成偶发失败。
#[track_caller]
fn wait_for(
    view: &Entity<SettingsView>,
    cx: &mut VisualTestContext,
    description: &str,
    mut predicate: impl FnMut(&SettingsView) -> bool,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        cx.run_until_parked();
        if view.update(cx, |view, _cx| predicate(view)) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "等待超时：{description}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn mount(
    cx: &mut TestAppContext,
    catalog: ModelCatalog,
    status: Option<String>,
) -> (Entity<SettingsView>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        gpui_tokio::init(cx);
    });
    cx.add_window_view(|_window, cx| SettingsView::new(catalog, unavailable_agent(), status, cx))
}

fn mount_settings_window(
    cx: &mut TestAppContext,
    catalog: ModelCatalog,
    status: Option<String>,
) -> (
    Entity<SettingsView>,
    Entity<SettingsWindowView>,
    &mut VisualTestContext,
) {
    cx.update(|cx| {
        gpui_component::init(cx);
        gpui_tokio::init(cx);
    });
    let view =
        cx.update(|cx| cx.new(|cx| SettingsView::new(catalog, unavailable_agent(), status, cx)));
    let config = view.clone();
    let (window, cx) =
        cx.add_window_view(move |window, cx| SettingsWindowView::new(config, window, cx));
    (view, window, cx)
}
