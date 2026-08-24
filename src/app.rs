//! 扫描启动资源、初始化 GPUI，并组合桌宠窗口中的应用实体。

use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use async_channel::Receiver;
use gpui::{
    App, AppContext, AssetSource, Entity, QuitMode, Result as GpuiResult, SharedString, Styled,
    WindowBackgroundAppearance, WindowDecorations, WindowKind, WindowOptions, px, size,
    transparent_black,
};
use gpui_component::Root;
use gpui_platform::application;
use gpui_tokio::Tokio;
use lunamate_agent::{
    Agent, AgentMemory, Client, chat_limits, client_from_model, model_and_options_from_config,
    persistence::{
        AgentPersistenceCallbacks, PersistenceError, PersistentMemoryTier, PersistentMemoryUsage,
        SessionDocument,
    },
};
use parking_lot::Mutex;
use rust_i18n::t;

use crate::{
    config::{CONFIG, ConfigWindow},
    database::{Database, DatabaseError, MemoryTier},
    model::ModelCatalog,
    platform::{APPLICATION_ID, SystemTray, SystemTrayAction, configure_desktop_pet_window},
    ui::{
        AgentView, DesktopPetView, SettingsView, apply, apply_language,
        desktop_pet_window_min_size, desktop_pet_window_size, raster_dimensions_for_window,
        restored_window_bounds,
    },
    voice::{SpeechPlayback, SpeechPlaybackShutdown, VoiceController, VoiceShutdown},
};

const MODELS_DIRECTORY: &str = "models";
const ASYNC_WORKER_THREADS: usize = 2;
const FINAL_AGENT_SAVE_TIMEOUT: Duration = Duration::from_secs(5);
const VOICE_SHUTDOWN_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const AGENT_DOCUMENT_SCOPE: &str = "agent";
const AGENT_SESSION_KEY_PREFIX: &str = "chat-session/";

type FinalAgentSave = Arc<Mutex<Option<tokio::task::JoinHandle<Result<(), String>>>>>;

fn agent_session_key(persona: &str) -> String {
    format!("{AGENT_SESSION_KEY_PREFIX}{persona}")
}

fn agent_persistence_error(error: DatabaseError) -> PersistenceError {
    let diagnostic_kind = error.diagnostic_kind();
    if matches!(&error, DatabaseError::InvalidStoredDocument) {
        PersistenceError::invalid_document(diagnostic_kind, error)
    } else {
        PersistenceError::new(diagnostic_kind, error)
    }
}

fn agent_persistence_callbacks(
    database: Result<Arc<Database>, DatabaseError>,
) -> Result<AgentPersistenceCallbacks, PersistenceError> {
    let database = database.map_err(agent_persistence_error)?;
    let load_database = database.clone();
    let save_database = database.clone();
    let delete_database = database.clone();
    let usage_database = database.clone();
    Ok(AgentPersistenceCallbacks::new(
        move |persona| {
            let database = load_database.clone();
            async move {
                database
                    .read_document(AGENT_DOCUMENT_SCOPE, &agent_session_key(&persona))
                    .await
                    .map(|document| {
                        document.map(|document| {
                            SessionDocument::new(
                                document.format_version(),
                                document.contents().to_vec(),
                            )
                        })
                    })
                    .map_err(agent_persistence_error)
            }
        },
        move |persona, document| {
            let database = save_database.clone();
            async move {
                let (format_version, contents) = document.into_parts();
                database
                    .write_document(
                        AGENT_DOCUMENT_SCOPE,
                        &agent_session_key(&persona),
                        format_version,
                        &contents,
                    )
                    .await
                    .map_err(agent_persistence_error)
            }
        },
        move |persona| {
            let database = delete_database.clone();
            async move {
                database
                    .delete_document(AGENT_DOCUMENT_SCOPE, &agent_session_key(&persona))
                    .await
                    .map_err(agent_persistence_error)
            }
        },
        move |persona| {
            let database = usage_database.clone();
            async move {
                database
                    .agent_memory_usage(&persona)
                    .await
                    .map(|usage| PersistentMemoryUsage::new(usage.medium, usage.long))
                    .map_err(agent_persistence_error)
            }
        },
        move |persona, tier| {
            let database = database.clone();
            async move {
                let tier = tier.map(|tier| match tier {
                    PersistentMemoryTier::Medium => MemoryTier::Medium,
                    PersistentMemoryTier::Long => MemoryTier::Long,
                });
                database
                    .delete_agent_memories(&persona, tier)
                    .await
                    .map_err(agent_persistence_error)
            }
        },
    ))
}

/// 把资源路径与编译期内容绑定在一处，避免新增图标时漏改其中一侧。
macro_rules! app_assets {
    ($($path:literal),+ $(,)?) => {
        &[$((
            concat!("icons/", $path),
            include_bytes!(concat!("../assets/icons/", $path)) as &[u8],
        )),+]
    };
}

const APP_ASSETS: &[(&str, &[u8])] = app_assets![
    "bot.svg",
    "check.svg",
    "chevron-down.svg",
    "chevron-right.svg",
    "copy.svg",
    "eye-off.svg",
    "folder-open.svg",
    "grip-vertical.svg",
    "image-plus.svg",
    "keyboard.svg",
    "message-circle.svg",
    "mic.svg",
    "minus.svg",
    "move.svg",
    "pencil.svg",
    "play.svg",
    "plus.svg",
    "refresh-cw.svg",
    "send.svg",
    "settings.svg",
    "square.svg",
    "trash-2.svg",
    "triangle-alert.svg",
    "user-round.svg",
    "x.svg",
    "providers/aihubmix.svg",
    "providers/aliyun.svg",
    "providers/anthropic.svg",
    "providers/baidu.svg",
    "providers/bedrock-api-key.svg",
    "providers/bigmodel.svg",
    "providers/cohere.svg",
    "providers/deepseek.svg",
    "providers/fireworks.svg",
    "providers/gemini.svg",
    "providers/github-models.svg",
    "providers/groq.svg",
    "providers/mimo.svg",
    "providers/minimax.svg",
    "providers/moonshot.svg",
    "providers/nebius.svg",
    "providers/ollama.svg",
    "providers/ollama-cloud.svg",
    "providers/openai.svg",
    "providers/openai-responses.svg",
    "providers/opencode-go.svg",
    "providers/openrouter.svg",
    "providers/together.svg",
    "providers/vertex.svg",
    "providers/xai.svg",
    "providers/zai.svg",
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

    working_directory_models
        .or_else(|| executable_directory.map(|directory| directory.join(MODELS_DIRECTORY)))
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

fn spawn_final_agent_save(
    agent: Arc<Agent>,
    runtime: &tokio::runtime::Handle,
    final_save: &FinalAgentSave,
) {
    let task = runtime.spawn(async move { agent.shutdown().await });
    *final_save.lock() = Some(task);
}

fn create_system_tray(
    runtime: &tokio::runtime::Handle,
) -> Option<(Rc<SystemTray>, Receiver<SystemTrayAction>)> {
    match SystemTray::install(runtime, CONFIG.use_native_tray_menu()) {
        Ok((tray, actions)) => {
            log::info!(
                "event=tray_controller_created native_menu={} style_choice_supported={}",
                tray.uses_native_menu(),
                SystemTray::supports_menu_style_choice()
            );
            Some((Rc::new(tray), actions))
        }
        Err(_) => {
            log::warn!("event=tray_init_failed stage=controller");
            None
        }
    }
}

fn listen_for_system_tray_actions(
    model_view: &Entity<DesktopPetView>,
    actions: Receiver<SystemTrayAction>,
    cx: &mut App,
) {
    let model_for_tray = model_view.downgrade();

    cx.spawn(async move |cx| {
        while let Ok(action) = actions.recv().await {
            match action {
                SystemTrayAction::ToggleDesktopPet => {
                    match model_for_tray.update_in(cx, |model, window, cx| {
                        model.toggle_desktop_pet_visibility(window, cx)
                    }) {
                        Ok(Ok(visible)) => {
                            log::info!(
                                "event=tray_action_completed action=toggle_desktop_pet visible={}",
                                visible
                            );
                        }
                        Ok(Err(_)) => {
                            let _ = model_for_tray.update(cx, |model, _| {
                                model.sync_desktop_pet_visibility_to_tray();
                            });
                            log::warn!(
                                "event=tray_action_failed action=toggle_desktop_pet stage=visibility"
                            );
                        }
                        Err(_) => break,
                    }
                }
                SystemTrayAction::OpenSettings => {
                    log::info!("event=tray_action_received action=open_settings");
                    if model_for_tray
                        .update(cx, |model, cx| model.open_config_window(cx))
                        .is_err()
                    {
                        break;
                    }
                }
                SystemTrayAction::OpenMenu(anchor) => {
                    log::debug!("event=tray_action_received action=open_menu");
                    if model_for_tray
                        .update(cx, |model, cx| model.toggle_tray_menu(anchor, cx))
                        .is_err()
                    {
                        break;
                    }
                }
                SystemTrayAction::Quit => {
                    log::info!("event=tray_action_received action=quit");
                    cx.update(|cx| cx.quit());
                    break;
                }
            }
        }
    })
    .detach();
}

/// 启动 LunaMate 应用并运行 GPUI 事件循环。
///
pub(super) fn run() {
    apply_language(CONFIG.appearance().language);
    let models_root = models_directory();
    let configured_model = CONFIG.selected_model();
    let model_catalog = ModelCatalog::empty(models_root);
    let config_status = join_status(
        CONFIG
            .startup_warning()
            .map(|warning| t!("status.startup_warning", warning = warning).to_string()),
        Some(t!("status.scanning_models").to_string()),
    );
    let async_runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(ASYNC_WORKER_THREADS)
        .thread_name("lunamate-async")
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            log::error!("event=async_runtime_init_failed");
            return;
        }
    };
    log::info!("event=async_runtime_ready worker_threads={ASYNC_WORKER_THREADS}");
    let database = async_runtime.block_on(Database::open_default());
    let persistence = agent_persistence_callbacks(database);
    let snapshot = CONFIG.agent_config_snapshot();
    let persona = snapshot
        .personas()
        .active()
        .expect("Agent 配置快照必须至少包含一个经过校验的人格");
    let model = persona
        .model
        .as_deref()
        .and_then(|id| snapshot.settings().model(id))
        .or_else(|| snapshot.settings().selected())
        .cloned();
    let client = model
        .as_ref()
        .map_or_else(Client::default, client_from_model);
    let (model_iden, options) = model
        .as_ref()
        .and_then(model_and_options_from_config)
        .map_or((None, None), |(model, options)| (Some(model), options));
    let limits = chat_limits(persona, snapshot.settings());
    let (agent_memory, agent_status) = match persistence {
        Ok(persistence) => (AgentMemory::new(Some(persistence)), None),
        Err(error) => {
            log::error!(
                "event=database_init_failed error_kind={}",
                error.diagnostic_kind()
            );
            (
                AgentMemory::unavailable(),
                Some(
                    t!(
                        "chat.persistence_unavailable",
                        locale = snapshot.language().id(),
                        error = error.diagnostic_kind()
                    )
                    .to_string(),
                ),
            )
        }
    };
    let agent = async_runtime.block_on(Agent::load(
        client,
        model_iden,
        options,
        persona.system_prompt.clone(),
        agent_memory,
        persona.id.clone(),
        limits,
        snapshot.language(),
        agent_status,
    ));
    let agent_generation = snapshot.generation();
    let async_handle = async_runtime.handle().clone();
    let final_agent_save: FinalAgentSave = Arc::new(Mutex::new(None));
    let final_agent_save_for_app = final_agent_save.clone();
    let async_handle_for_app = async_handle.clone();
    let (voice_controller, voice_shutdown): (Option<VoiceController>, Option<VoiceShutdown>) =
        match VoiceController::start(Arc::new(
            CONFIG
                .voice_settings()
                .runtime(CONFIG.llm_settings().as_ref()),
        )) {
            Ok((controller, shutdown)) => (Some(controller), Some(shutdown)),
            Err(_) => {
                log::error!("event=voice_service_start_failed");
                (None, None)
            }
        };
    let voice_for_app = voice_controller.clone();
    let (speech_playback, speech_playback_shutdown): (
        Option<SpeechPlayback>,
        Option<SpeechPlaybackShutdown>,
    ) = match SpeechPlayback::start() {
        Ok((playback, shutdown)) => (Some(playback), Some(shutdown)),
        Err(_) => {
            log::warn!("event=speech_playback_unavailable stage=start");
            (None, None)
        }
    };
    let speech_playback_for_app = speech_playback.clone();

    application()
        .with_assets(AppAssets)
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(move |cx: &mut App| {
            gpui_tokio::init_from_handle(cx, async_handle.clone());
            gpui_component::init(cx);
            let appearance = CONFIG.appearance();
            apply(&appearance, None, cx);
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
            let final_agent_save = final_agent_save_for_app.clone();
            let async_handle = async_handle_for_app.clone();
            let voice = voice_for_app.clone();
            let speech_playback = speech_playback_for_app.clone();

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
                    app_id: Some(APPLICATION_ID.to_owned()),
                    ..Default::default()
                },
                move |window, cx| {
                    if configure_desktop_pet_window(window).is_err() {
                        log::warn!("event=desktop_pet_window_config_failed");
                    }
                    let raster_dimensions = raster_dimensions_for_window(
                        window_width,
                        window_height,
                        window.scale_factor(),
                    );
                    let config = cx.new(|cx| {
                        SettingsView::new(model_catalog, agent.clone(), config_status, cx)
                    });
                    let system_tray = create_system_tray(&async_handle);
                    let config_for_quit = config.downgrade();
                    let agent_view =
                        cx.new(|cx| AgentView::new(agent.clone(), agent_generation, window, cx));
                    agent_view.update(cx, |view, cx| view.start_initial_reply_fade(cx));
                    let agent_for_quit = agent.clone();
                    let agent_for_window_close = agent.clone();
                    let final_agent_save_for_quit = final_agent_save.clone();
                    let async_handle_for_quit = async_handle.clone();
                    cx.on_app_quit(move |cx| {
                        log::info!("event=app_shutdown_started");
                        let config_tasks = config_for_quit
                            .update(cx, |config, cx| config.take_pending_write_tasks(cx))
                            .unwrap_or_default();
                        spawn_final_agent_save(
                            agent_for_quit.clone(),
                            &async_handle_for_quit,
                            &final_agent_save_for_quit,
                        );
                        let persistence_task = Tokio::spawn(cx, async move {
                            tokio::task::spawn_blocking(|| {
                                CONFIG
                                    .persist_window_positions()
                                    .map_err(|error| error.to_string())
                            })
                            .await
                            .unwrap_or_else(|error| Err(error.to_string()))
                        });
                        async move {
                            for task in config_tasks {
                                task.await;
                            }
                            let window_result = persistence_task
                                .await
                                .unwrap_or_else(|error| Err(error.to_string()));
                            if window_result.is_err() {
                                log::error!("event=window_position_save_failed phase=shutdown");
                            }
                        }
                    })
                    .detach();
                    let model_view = cx.new(|cx| {
                        DesktopPetView::new(
                            config.clone(),
                            agent_view,
                            voice,
                            speech_playback,
                            None,
                            raster_dimensions,
                            system_tray.as_ref().map(|(tray, _)| Rc::clone(tray)),
                            &async_handle,
                            window,
                            cx,
                        )
                    });
                    config.update(cx, |config, cx| {
                        config.start_initial_scan(configured_model, window, cx);
                    });
                    let model_for_window_close = model_view.downgrade();
                    let final_agent_save_for_window_close = final_agent_save.clone();
                    let async_handle_for_window_close = async_handle.clone();
                    window.on_window_should_close(cx, move |_, cx| {
                        spawn_final_agent_save(
                            agent_for_window_close.clone(),
                            &async_handle_for_window_close,
                            &final_agent_save_for_window_close,
                        );
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
                    if let Some((_, actions)) = system_tray {
                        listen_for_system_tray_actions(&model_view, actions, cx);
                    }
                    // Root 默认铺满主题背景，需覆盖为透明色才能保留交换链的 Alpha。
                    cx.new(|cx| {
                        Root::new(model_view, window, cx)
                            .bordered(false)
                            .bg(transparent_black())
                    })
                },
            );
            match result {
                Ok(_) => log::info!("event=desktop_pet_window_created"),
                Err(_) => {
                    log::error!("event=desktop_pet_window_create_failed");
                    cx.quit();
                }
            }
        });

    let voice_shutdown_completed = voice_shutdown
        .is_none_or(|voice_shutdown| voice_shutdown.shutdown(VOICE_SHUTDOWN_WAIT_TIMEOUT));
    let playback_shutdown_completed = speech_playback_shutdown
        .is_none_or(|shutdown| shutdown.shutdown(VOICE_SHUTDOWN_WAIT_TIMEOUT));

    let final_save = final_agent_save.lock().take();
    if let Some(final_save) = final_save {
        let result = async_runtime.block_on(async move {
            let mut final_save = final_save;
            match tokio::time::timeout(FINAL_AGENT_SAVE_TIMEOUT, &mut final_save).await {
                Ok(result) => result.unwrap_or_else(|error| Err(error.to_string())),
                Err(_) => {
                    final_save.abort();
                    Err(format!(
                        "等待最终会话保存超过 {} 秒",
                        FINAL_AGENT_SAVE_TIMEOUT.as_secs()
                    ))
                }
            }
        });
        if result.is_err() {
            log::error!("event=chat_session_save_failed phase=shutdown");
        } else {
            log::debug!("event=chat_session_saved phase=shutdown");
        }
    }
    if voice_shutdown_completed && playback_shutdown_completed {
        log::info!("event=runtime_resources_released");
    } else {
        log::warn!("event=runtime_resource_shutdown_incomplete subsystem=audio");
    }
}
