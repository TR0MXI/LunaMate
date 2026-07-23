//! 扫描启动资源、初始化 GPUI，并组合桌宠窗口中的应用实体。

use std::{
    borrow::Cow,
    path::{Path, PathBuf},
};

use gpui::{
    App, AppContext, AssetSource, QuitMode, Result as GpuiResult, SharedString, Styled,
    WindowBackgroundAppearance, WindowDecorations, WindowKind, WindowOptions, px, size,
    transparent_black,
};
use gpui_component::Root;
use gpui_platform::application;
use rust_i18n::t;

use crate::{
    chat::{ChatSession, ChatSessionStore, ChatView},
    config::{CONFIG, ConfigView, ConfigWindow, ModelCatalog},
    model_view::ModelView,
    platform_window::configure_desktop_pet_window,
    theme,
    window::{
        desktop_pet_window_min_size, desktop_pet_window_size, raster_dimensions_for_window,
        restored_window_bounds,
    },
};

const MODELS_DIRECTORY: &str = "models";

const APP_ASSETS: &[(&str, &[u8])] = &[
    (
        "icons/bot.svg",
        include_bytes!("../../assets/icons/bot.svg"),
    ),
    (
        "icons/check.svg",
        include_bytes!("../../assets/icons/check.svg"),
    ),
    (
        "icons/message-circle.svg",
        include_bytes!("../../assets/icons/message-circle.svg"),
    ),
    (
        "icons/move.svg",
        include_bytes!("../../assets/icons/move.svg"),
    ),
    (
        "icons/play.svg",
        include_bytes!("../../assets/icons/play.svg"),
    ),
    (
        "icons/plus.svg",
        include_bytes!("../../assets/icons/plus.svg"),
    ),
    (
        "icons/refresh-cw.svg",
        include_bytes!("../../assets/icons/refresh-cw.svg"),
    ),
    (
        "icons/settings.svg",
        include_bytes!("../../assets/icons/settings.svg"),
    ),
    (
        "icons/send.svg",
        include_bytes!("../../assets/icons/send.svg"),
    ),
    (
        "icons/square.svg",
        include_bytes!("../../assets/icons/square.svg"),
    ),
    (
        "icons/trash-2.svg",
        include_bytes!("../../assets/icons/trash-2.svg"),
    ),
    ("icons/x.svg", include_bytes!("../../assets/icons/x.svg")),
];

/// 向 GPUI 提供编译进可执行文件的应用图标。
struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> GpuiResult<Option<Cow<'static, [u8]>>> {
        Ok(APP_ASSETS
            .iter()
            .find_map(|(asset_path, bytes)| (*asset_path == path).then_some(Cow::Borrowed(*bytes))))
    }

    fn list(&self, path: &str) -> GpuiResult<Vec<SharedString>> {
        Ok(APP_ASSETS
            .iter()
            .filter(|(asset_path, _)| asset_path.starts_with(path))
            .map(|(asset_path, _)| SharedString::from(*asset_path))
            .collect())
    }
}

fn models_directory() -> PathBuf {
    let executable_directory = std::env::current_exe()
        .ok()
        .and_then(|executable| executable.parent().map(Path::to_path_buf));
    if let Some(directory) = &executable_directory {
        for ancestor in directory.ancestors().take(3) {
            let candidate = ancestor.join(MODELS_DIRECTORY);
            if candidate.is_dir() {
                return candidate;
            }
        }
    }

    let working_directory_models = std::env::current_dir()
        .ok()
        .map(|directory| directory.join(MODELS_DIRECTORY));
    if let Some(path) = &working_directory_models
        && path.is_dir()
    {
        return path.clone();
    }

    executable_directory
        .map(|directory| directory.join(MODELS_DIRECTORY))
        .or(working_directory_models)
        .unwrap_or_else(|| PathBuf::from(MODELS_DIRECTORY))
}

fn join_status(first: Option<String>, second: Option<String>) -> Option<String> {
    match (first, second) {
        (Some(first), Some(second)) => {
            Some(format!("{first}{}{second}", t!("common.status_separator")))
        }
        (Some(status), None) | (None, Some(status)) => Some(status),
        (None, None) => None,
    }
}

/// 启动 LunaMate 应用并运行 GPUI 事件循环。
///
pub(super) fn run() {
    theme::apply_language(CONFIG.appearance().language);
    let models_root = models_directory();
    let configured_model = CONFIG.selected_model();
    let model_catalog = ModelCatalog::empty(models_root);
    let config_status = join_status(
        CONFIG
            .startup_warning()
            .map(|warning| t!("status.startup_warning", warning = warning).to_string()),
        Some(t!("status.scanning_models").to_string()),
    );
    let session_path = CONFIG.chat_session_path();
    let (chat_session, chat_store, chat_status) = match ChatSessionStore::load(session_path.clone())
    {
        Ok((session, store)) => (session, store, None),
        Err(error) => (
            ChatSession::default(),
            ChatSessionStore::empty(session_path),
            Some(t!("chat.restore_failed", error = error.to_string()).to_string()),
        ),
    };

    application()
        .with_assets(AppAssets)
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(move |cx: &mut App| {
            gpui_tokio::init(cx);
            gpui_component::init(cx);
            let appearance = CONFIG.appearance();
            theme::apply(&appearance, None, cx);
            let display_size = cx
                .primary_display()
                .map(|display| display.visible_bounds().size)
                .unwrap_or_else(|| size(px(1280.0), px(720.0)));
            let display_width = f32::from(display_size.width);
            let display_height = f32::from(display_size.height);
            let [window_width, window_height] =
                desktop_pet_window_size(display_width, display_height, CONFIG.model_window_size());
            let window_size = size(px(window_width), px(window_height));
            let window_min_size = desktop_pet_window_min_size(display_width, display_height);

            let result = cx.open_window(
                WindowOptions {
                    window_bounds: Some(restored_window_bounds(
                        ConfigWindow::DesktopPet,
                        window_size,
                        cx,
                    )),
                    window_min_size: Some(window_min_size),
                    titlebar: None,
                    kind: WindowKind::PopUp,
                    window_background: WindowBackgroundAppearance::Transparent,
                    window_decorations: Some(WindowDecorations::Client),
                    is_resizable: false,
                    is_minimizable: false,
                    is_movable: true,
                    app_id: Some("lunamate".to_owned()),
                    ..Default::default()
                },
                move |window, cx| {
                    if let Err(error) = configure_desktop_pet_window(window) {
                        eprintln!("配置桌宠原生窗口失败：{error}");
                    }
                    let raster_dimensions = raster_dimensions_for_window(
                        window_width,
                        window_height,
                        window.scale_factor(),
                    );
                    let config = cx.new(|cx| ConfigView::new(model_catalog, config_status, cx));
                    let config_for_quit = config.downgrade();
                    let chat = cx.new(|cx| {
                        ChatView::new(
                            CONFIG.llm_settings(),
                            chat_session,
                            chat_store,
                            chat_status,
                            window,
                            cx,
                        )
                    });
                    let chat_for_quit = chat.downgrade();
                    cx.on_app_quit(move |cx| {
                        let config_tasks = config_for_quit
                            .update(cx, |config, cx| config.take_pending_write_tasks(cx))
                            .unwrap_or_default();
                        let chat_snapshot = chat_for_quit
                            .update(cx, |chat, _| chat.shutdown_snapshot())
                            .ok();
                        let background = cx.background_executor().clone();
                        let persistence_task = background.spawn(async move {
                            let chat_result = chat_snapshot
                                .map(|(store, snapshot)| store.save(snapshot))
                                .transpose();
                            (chat_result, CONFIG.persist_window_positions())
                        });
                        async move {
                            for task in config_tasks {
                                task.await;
                            }
                            let (chat_result, window_result) = persistence_task.await;
                            if let Err(error) = chat_result {
                                eprintln!("应用退出时保存聊天会话失败：{error}");
                            }
                            if let Err(error) = window_result {
                                eprintln!("应用退出时保存窗口位置失败：{error}");
                            }
                        }
                    })
                    .detach();
                    let model_view = cx.new(|cx| {
                        ModelView::new(config.clone(), chat, None, raster_dimensions, window, cx)
                    });
                    config.update(cx, |config, cx| {
                        config.start_initial_scan(configured_model, cx);
                    });
                    let model_for_window_close = model_view.downgrade();
                    window.on_window_should_close(cx, move |_, cx| {
                        model_for_window_close
                            .update(cx, |model, cx| model.request_window_close(cx))
                            .unwrap_or(true)
                    });
                    let model_for_quit = model_view.downgrade();
                    cx.on_app_quit(move |cx| {
                        let _ = model_for_quit.update(cx, |model, _| {
                            model.shutdown_gpu_for_quit();
                        });
                        async {}
                    })
                    .detach();
                    // Root 默认铺满主题背景，需覆盖为透明色才能保留交换链的 Alpha。
                    cx.new(|cx| {
                        Root::new(model_view, window, cx)
                            .bordered(false)
                            .bg(transparent_black())
                    })
                },
            );
            if let Err(error) = result {
                eprintln!("无法创建 LunaMate 窗口：{error}");
                cx.quit();
            }
        });
}
