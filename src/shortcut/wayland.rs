//! 通过 XDG GlobalShortcuts portal 管理原生 Wayland 快捷键 session。

use std::{collections::HashSet, time::Duration};

use ashpd::{
    AppID,
    desktop::{
        CreateSessionOptions, ResponseError, Session,
        global_shortcuts::{
            BindShortcutsOptions, GlobalShortcuts, ListShortcutsOptions, NewShortcut,
        },
    },
};
use async_channel::{Receiver, Sender};
use futures::StreamExt as _;
use global_hotkey::{
    HotKeyState,
    hotkey::{Code, Modifiers},
};
use rust_i18n::t;
use tokio::{runtime::Handle, sync::watch, task::JoinHandle};

use crate::{
    config::{KeyboardShortcut, ShortcutAction, ShortcutSettings},
    platform::{APPLICATION_ID, WaylandActivationController, WaylandActivationTarget},
};

use super::{ShortcutEvent, ShortcutRuntimeBinding};

const EVENT_CHANNEL_CAPACITY: usize = 32;
const RECONFIGURE_DELAY: Duration = Duration::from_millis(150);
const PORTAL_RETRY_DELAY: Duration = Duration::from_secs(5);
const PORTAL_HEALTH_INTERVAL: Duration = Duration::from_secs(30);
const PORTAL_HEALTH_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct WaylandShortcutManager {
    state: watch::Sender<PortalState>,
    task: JoinHandle<()>,
    activation: WaylandActivationController,
}

#[derive(Clone)]
struct PortalState {
    revision: u64,
    settings: ShortcutSettings,
    suspended: bool,
    shutdown: bool,
}

enum PortalConnectionExit {
    Shutdown,
    Reconfigure,
}

impl WaylandShortcutManager {
    pub(super) fn new(
        settings: ShortcutSettings,
        target: WaylandActivationTarget,
        runtime: &Handle,
    ) -> (Self, Receiver<ShortcutEvent>) {
        let initial = PortalState {
            revision: 1,
            settings,
            suspended: false,
            shutdown: false,
        };
        let (state, state_receiver) = watch::channel(initial);
        let (event_sender, events) = async_channel::bounded(EVENT_CHANNEL_CAPACITY);
        let task = runtime.spawn(run_worker(state_receiver, event_sender));
        let activation = WaylandActivationController::start(target);
        (
            Self {
                state,
                task,
                activation,
            },
            events,
        )
    }

    pub(super) fn configure(&self, settings: ShortcutSettings) {
        self.state.send_modify(|state| {
            if state.settings != settings {
                state.settings = settings;
                state.revision = state.revision.wrapping_add(1).max(1);
            }
        });
    }

    pub(super) fn set_suspended(&self, suspended: bool) {
        self.state.send_modify(|state| {
            if state.suspended != suspended {
                state.suspended = suspended;
                state.revision = state.revision.wrapping_add(1).max(1);
            }
        });
    }

    pub(super) fn current_revision(&self) -> u64 {
        self.state.borrow().revision
    }

    pub(super) fn activate(&self, token: String) -> Result<(), String> {
        self.activation.activate(token)
    }
}

impl Drop for WaylandShortcutManager {
    fn drop(&mut self) {
        self.state.send_modify(|state| {
            state.shutdown = true;
            state.revision = state.revision.wrapping_add(1).max(1);
        });
        self.task.abort();
    }
}

async fn run_worker(
    mut state_receiver: watch::Receiver<PortalState>,
    events: Sender<ShortcutEvent>,
) {
    loop {
        if state_receiver.borrow().shutdown {
            return;
        }
        match run_portal_connection(&mut state_receiver, &events).await {
            Ok(PortalConnectionExit::Shutdown) => return,
            Ok(PortalConnectionExit::Reconfigure) => continue,
            Err(error) => {
                let revision = state_receiver.borrow().revision;
                if !send_runtime_bindings(&events, revision, Vec::new()).await {
                    return;
                }
                if !send_runtime_errors(&events, revision, vec![error]).await {
                    return;
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(PORTAL_RETRY_DELAY) => {}
            changed = state_receiver.changed() => {
                if changed.is_err() || state_receiver.borrow().shutdown {
                    return;
                }
            }
        }
    }
}

async fn run_portal_connection(
    state_receiver: &mut watch::Receiver<PortalState>,
    events: &Sender<ShortcutEvent>,
) -> Result<PortalConnectionExit, String> {
    let connection = ashpd::zbus::Connection::session()
        .await
        .map_err(|error| format!("连接 XDG GlobalShortcuts portal 失败：{error}"))?;
    let app_id = AppID::try_from(APPLICATION_ID)
        .map_err(|error| format!("全局快捷键应用标识无效：{error}"))?;
    match ashpd::register_host_app_with_connection(connection.clone(), app_id).await {
        Ok(()) => {}
        Err(ashpd::Error::PortalNotFound(_)) => {
            log::warn!("当前 portal 没有 host Registry，尝试沿用 portal 识别的应用标识");
        }
        Err(error) => return Err(format!("注册全局快捷键 host 应用标识失败：{error}")),
    }
    let portal = GlobalShortcuts::with_connection(connection)
        .await
        .map_err(|error| format!("创建 XDG GlobalShortcuts proxy 失败：{error}"))?;
    let mut activated = Box::pin(
        portal
            .receive_activated()
            .await
            .map_err(|error| format!("订阅 Wayland 快捷键按下事件失败：{error}"))?,
    );
    let mut deactivated = Box::pin(
        portal
            .receive_deactivated()
            .await
            .map_err(|error| format!("订阅 Wayland 快捷键松开事件失败：{error}"))?,
    );
    let mut shortcuts_changed = Box::pin(
        portal
            .receive_shortcuts_changed()
            .await
            .map_err(|error| format!("订阅 Wayland 快捷键变更事件失败：{error}"))?,
    );
    loop {
        let state = state_receiver.borrow().clone();
        if state.shutdown {
            return Ok(PortalConnectionExit::Shutdown);
        }
        if state.suspended || state.settings.configured_count() == 0 {
            if !send_runtime_bindings(events, state.revision, Vec::new()).await {
                return Ok(PortalConnectionExit::Shutdown);
            }
            if !send_runtime_errors(events, state.revision, Vec::new()).await {
                return Ok(PortalConnectionExit::Shutdown);
            }
            if state_receiver.changed().await.is_err() {
                return Ok(PortalConnectionExit::Shutdown);
            }
            continue;
        }

        let revision = state.revision;
        if !send_runtime_bindings(events, revision, Vec::new()).await {
            return Ok(PortalConnectionExit::Shutdown);
        }
        tokio::select! {
            _ = tokio::time::sleep(RECONFIGURE_DELAY) => {}
            changed = state_receiver.changed() => {
                if changed.is_err() {
                    return Ok(PortalConnectionExit::Shutdown);
                }
                continue;
            }
        }
        if state_receiver.borrow().revision != revision {
            continue;
        }

        let session = tokio::select! {
            biased;
            changed = state_receiver.changed() => {
                return Ok(connection_exit_after_change(changed, state_receiver));
            }
            result = portal.create_session(CreateSessionOptions::default()) => {
                result.map_err(|error| format!("创建 Wayland 快捷键 session 失败：{error}"))?
            }
        };
        if state_receiver.borrow().revision != revision {
            close_session(&session).await?;
            return Ok(PortalConnectionExit::Reconfigure);
        }
        let session_path = serialized_session_path(&session)?;
        let session_closed_result = tokio::select! {
            biased;
            changed = state_receiver.changed() => {
                return Ok(connection_exit_after_change(changed, state_receiver));
            }
            result = session.receive_closed() => result,
        };
        let mut session_closed = match session_closed_result {
            Ok(stream) => Box::pin(stream),
            Err(error) => {
                close_session(&session).await?;
                return Err(format!("订阅 Wayland 快捷键 session 关闭事件失败：{error}"));
            }
        };

        let listed = tokio::select! {
            biased;
            changed = state_receiver.changed() => {
                return Ok(connection_exit_after_change(changed, state_receiver));
            }
            result = portal.list_shortcuts(&session, ListShortcutsOptions::default()) => result,
        };
        match listed {
            Ok(request) => match request.response() {
                Ok(response) => log::debug!(
                    "Wayland portal 已恢复快捷键：count={}",
                    response.shortcuts().len()
                ),
                Err(error) => log::warn!("读取 Wayland portal 已有快捷键失败：{error}"),
            },
            Err(error) => log::warn!("请求 Wayland portal 已有快捷键失败：{error}"),
        }
        if state_receiver.borrow().revision != revision {
            close_session(&session).await?;
            return Ok(PortalConnectionExit::Reconfigure);
        }

        let requested = requested_shortcuts(&state.settings)?;
        let bind_result = tokio::select! {
            biased;
            changed = state_receiver.changed() => {
                return Ok(connection_exit_after_change(changed, state_receiver));
            }
            result = portal.bind_shortcuts(
                &session,
                &requested.descriptors,
                None,
                BindShortcutsOptions::default(),
            ) => result,
        };
        let response = bind_result.and_then(|request| request.response());
        if state_receiver.borrow().revision != revision {
            close_session(&session).await?;
            return Ok(PortalConnectionExit::Reconfigure);
        }
        let response = match response {
            Ok(response) => response,
            Err(ashpd::Error::Response(ResponseError::Cancelled)) => {
                let _ = send_runtime_errors(
                    events,
                    revision,
                    vec!["Wayland 快捷键授权已取消；重新录入任意快捷键可再次授权".to_owned()],
                )
                .await;
                close_session(&session).await?;
                if state_receiver.changed().await.is_err() {
                    return Ok(PortalConnectionExit::Shutdown);
                }
                continue;
            }
            Err(error) => {
                close_session(&session).await?;
                return Err(format!("绑定 Wayland 快捷键失败：{error}"));
            }
        };
        if !send_runtime_bindings(events, revision, runtime_bindings(response.shortcuts())).await {
            close_session(&session).await?;
            return Ok(PortalConnectionExit::Shutdown);
        }
        let mut bindings = ActiveBindings::new(bound_actions(response.shortcuts()));
        if !send_runtime_errors(
            events,
            revision,
            missing_binding_errors(&requested.actions, bindings.bound()),
        )
        .await
        {
            close_session(&session).await?;
            return Ok(PortalConnectionExit::Shutdown);
        }
        let mut restart_error = None;
        let mut portal_closed = false;
        let mut health = tokio::time::interval_at(
            tokio::time::Instant::now() + PORTAL_HEALTH_INTERVAL,
            PORTAL_HEALTH_INTERVAL,
        );
        health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                changed = state_receiver.changed() => {
                    if changed.is_err() || state_receiver.borrow().shutdown {
                        break;
                    }
                    break;
                }
                event = activated.next() => {
                    let Some(event) = event else {
                        restart_error = Some("Wayland 快捷键按下事件流已结束".to_owned());
                        break;
                    };
                    if event.session_handle().as_str() != session_path {
                        continue;
                    }
                    let Some(action) = ShortcutAction::from_id(event.shortcut_id()) else {
                        log::warn!("Wayland portal 返回未知快捷键动作：{}", event.shortcut_id());
                        continue;
                    };
                    if !bindings.press(action) {
                        continue;
                    }
                    let activation_token = activation_token(event.options()).map(ToOwned::to_owned);
                    if !send_shortcut_event(
                        events,
                        ShortcutEvent::Portal {
                            revision,
                            action,
                            state: HotKeyState::Pressed,
                            activation_token,
                        },
                    )
                    .await
                    {
                        break;
                    }
                }
                event = deactivated.next() => {
                    let Some(event) = event else {
                        restart_error = Some("Wayland 快捷键松开事件流已结束".to_owned());
                        break;
                    };
                    if event.session_handle().as_str() != session_path {
                        continue;
                    }
                    let Some(action) = ShortcutAction::from_id(event.shortcut_id()) else {
                        continue;
                    };
                    if !bindings.release(action) {
                        continue;
                    }
                    if !send_shortcut_event(
                        events,
                        ShortcutEvent::Portal {
                            revision,
                            action,
                            state: HotKeyState::Released,
                            activation_token: None,
                        },
                    )
                    .await
                    {
                        break;
                    }
                }
                event = shortcuts_changed.next() => {
                    let Some(event) = event else {
                        restart_error = Some("Wayland 快捷键变更事件流已结束".to_owned());
                        break;
                    };
                    if event.session_handle().as_str() != session_path {
                        continue;
                    }
                    let next_actions = bound_actions(event.shortcuts());
                    for action in bindings.replace_bound(next_actions) {
                        if !send_shortcut_event(
                            events,
                            ShortcutEvent::Portal {
                                revision,
                                action,
                                state: HotKeyState::Released,
                                activation_token: None,
                            },
                        )
                        .await
                        {
                            break;
                        }
                    }
                    if !send_runtime_bindings(
                        events,
                        revision,
                        runtime_bindings(event.shortcuts()),
                    ).await {
                        break;
                    }
                    if !send_runtime_errors(
                        events,
                        revision,
                        missing_binding_errors(&requested.actions, bindings.bound()),
                    ).await {
                        break;
                    }
                }
                closed = session_closed.next() => {
                    let _ = closed;
                    restart_error = Some("Wayland 快捷键 session 已由 portal 关闭".to_owned());
                    portal_closed = true;
                    break;
                }
                _ = health.tick() => {
                    let result = tokio::select! {
                        biased;
                        changed = state_receiver.changed() => {
                            return Ok(connection_exit_after_change(changed, state_receiver));
                        }
                        result = tokio::time::timeout(
                            PORTAL_HEALTH_TIMEOUT,
                            portal.list_shortcuts(&session, ListShortcutsOptions::default()),
                        ) => result,
                    };
                    match result {
                        Ok(Ok(request)) => {
                            match request.response() {
                                Ok(response) => {
                                    let next_actions = bound_actions(response.shortcuts());
                                    for action in bindings.replace_bound(next_actions) {
                                        if !send_shortcut_event(
                                            events,
                                            ShortcutEvent::Portal {
                                                revision,
                                                action,
                                                state: HotKeyState::Released,
                                                activation_token: None,
                                            },
                                        ).await {
                                            break;
                                        }
                                    }
                                    if !send_runtime_bindings(
                                        events,
                                        revision,
                                        runtime_bindings(response.shortcuts()),
                                    ).await {
                                        break;
                                    }
                                    if !send_runtime_errors(
                                        events,
                                        revision,
                                        missing_binding_errors(
                                            &requested.actions,
                                            bindings.bound(),
                                        ),
                                    ).await {
                                        break;
                                    }
                                }
                                Err(error) => {
                                    restart_error = Some(format!(
                                        "Wayland 快捷键健康检查失败：{error}"
                                    ));
                                    break;
                                }
                            }
                        }
                        Ok(Err(error)) => {
                            restart_error = Some(format!("Wayland 快捷键健康检查失败：{error}"));
                            break;
                        }
                        Err(_) => {
                            restart_error = Some("Wayland 快捷键健康检查超时".to_owned());
                            break;
                        }
                    }
                }
            }
        }

        for action in bindings.take_pressed() {
            if !send_shortcut_event(
                events,
                ShortcutEvent::Portal {
                    revision,
                    action,
                    state: HotKeyState::Released,
                    activation_token: None,
                },
            )
            .await
            {
                break;
            }
        }
        if !portal_closed {
            close_session(&session).await?;
        }
        if events.is_closed() {
            return Ok(PortalConnectionExit::Shutdown);
        }
        if state_receiver.borrow().shutdown {
            return Ok(PortalConnectionExit::Shutdown);
        }
        if let Some(error) = restart_error {
            return Err(error);
        }
    }
}

struct RequestedShortcuts {
    descriptors: Vec<NewShortcut>,
    actions: HashSet<ShortcutAction>,
}

pub(super) struct ActiveBindings {
    bound: HashSet<ShortcutAction>,
    pressed: HashSet<ShortcutAction>,
}

impl ActiveBindings {
    pub(super) fn new(bound: HashSet<ShortcutAction>) -> Self {
        Self {
            bound,
            pressed: HashSet::new(),
        }
    }

    pub(super) fn bound(&self) -> &HashSet<ShortcutAction> {
        &self.bound
    }

    pub(super) fn press(&mut self, action: ShortcutAction) -> bool {
        self.bound.contains(&action) && self.pressed.insert(action)
    }

    pub(super) fn release(&mut self, action: ShortcutAction) -> bool {
        self.pressed.remove(&action)
    }

    pub(super) fn replace_bound(&mut self, next: HashSet<ShortcutAction>) -> Vec<ShortcutAction> {
        let released = self
            .pressed
            .iter()
            .copied()
            .filter(|action| !next.contains(action))
            .collect::<Vec<_>>();
        self.pressed.retain(|action| next.contains(action));
        self.bound = next;
        released
    }

    pub(super) fn take_pressed(&mut self) -> Vec<ShortcutAction> {
        self.pressed.drain().collect()
    }
}

fn requested_shortcuts(settings: &ShortcutSettings) -> Result<RequestedShortcuts, String> {
    let mut descriptors = Vec::with_capacity(settings.configured_count());
    let mut actions = HashSet::with_capacity(settings.configured_count());
    for action in ShortcutAction::ALL {
        let Some(shortcut) = settings.shortcut(action) else {
            continue;
        };
        let trigger = portal_trigger(shortcut)?;
        descriptors.push(
            NewShortcut::new(action.id(), action_description(action))
                .preferred_trigger(Some(trigger.as_str())),
        );
        actions.insert(action);
    }
    Ok(RequestedShortcuts {
        descriptors,
        actions,
    })
}

fn action_description(action: ShortcutAction) -> String {
    match action {
        ShortcutAction::VoiceInput => t!("shortcut.voice_input").to_string(),
        ShortcutAction::ToggleDesktopPet => t!("shortcut.toggle_desktop_pet").to_string(),
        ShortcutAction::ToggleSettings => t!("shortcut.toggle_settings").to_string(),
        ShortcutAction::ToggleChatInput => t!("shortcut.toggle_chat_input").to_string(),
    }
}

fn bound_actions(
    shortcuts: &[ashpd::desktop::global_shortcuts::Shortcut],
) -> HashSet<ShortcutAction> {
    shortcuts
        .iter()
        .filter_map(|shortcut| ShortcutAction::from_id(shortcut.id()))
        .collect()
}

fn runtime_bindings(
    shortcuts: &[ashpd::desktop::global_shortcuts::Shortcut],
) -> Vec<ShortcutRuntimeBinding> {
    shortcuts
        .iter()
        .filter_map(|shortcut| {
            let action = ShortcutAction::from_id(shortcut.id())?;
            Some(ShortcutRuntimeBinding::new(
                action,
                shortcut.trigger_description().to_owned(),
            ))
        })
        .collect()
}

fn missing_binding_errors(
    requested: &HashSet<ShortcutAction>,
    bound: &HashSet<ShortcutAction>,
) -> Vec<String> {
    requested
        .difference(bound)
        .map(|action| format!("{} 未获 Wayland 合成器授权", action.id()))
        .collect()
}

async fn send_runtime_errors(
    events: &Sender<ShortcutEvent>,
    revision: u64,
    errors: Vec<String>,
) -> bool {
    send_shortcut_event(events, ShortcutEvent::RuntimeErrors { revision, errors }).await
}

async fn send_runtime_bindings(
    events: &Sender<ShortcutEvent>,
    revision: u64,
    bindings: Vec<ShortcutRuntimeBinding>,
) -> bool {
    send_shortcut_event(
        events,
        ShortcutEvent::RuntimeBindings { revision, bindings },
    )
    .await
}

async fn send_shortcut_event(events: &Sender<ShortcutEvent>, event: ShortcutEvent) -> bool {
    events.send(event).await.is_ok()
}

async fn close_session(session: &Session<GlobalShortcuts>) -> Result<(), String> {
    session
        .close()
        .await
        .map_err(|error| format!("关闭 Wayland 快捷键 session 失败：{error}"))
}

fn connection_exit_after_change(
    changed: Result<(), watch::error::RecvError>,
    state_receiver: &watch::Receiver<PortalState>,
) -> PortalConnectionExit {
    if changed.is_err() || state_receiver.borrow().shutdown {
        PortalConnectionExit::Shutdown
    } else {
        PortalConnectionExit::Reconfigure
    }
}

fn activation_token(
    options: &std::collections::HashMap<String, ashpd::zbus::zvariant::OwnedValue>,
) -> Option<&str> {
    options
        .get("activation_token")
        .and_then(|value| <&str>::try_from(value).ok())
}

fn serialized_session_path<T: serde::Serialize>(session: &T) -> Result<String, String> {
    let value = serde_json::to_value(session)
        .map_err(|error| format!("序列化 Wayland 快捷键 session 失败：{error}"))?;
    value
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| "Wayland 快捷键 session 路径格式无效".to_owned())
}

pub(super) fn portal_trigger(shortcut: KeyboardShortcut) -> Result<String, String> {
    let hotkey = shortcut.hotkey();
    let mut parts = Vec::with_capacity(5);
    if hotkey.mods.contains(Modifiers::CONTROL) {
        parts.push("CTRL");
    }
    if hotkey.mods.contains(Modifiers::ALT) {
        parts.push("ALT");
    }
    if hotkey.mods.contains(Modifiers::SHIFT) {
        parts.push("SHIFT");
    }
    if hotkey.mods.contains(Modifiers::SUPER) {
        parts.push("LOGO");
    }
    let key = portal_key_name(hotkey.key)
        .ok_or_else(|| format!("快捷键无法转换为 XDG trigger：{}", shortcut.id()))?;
    parts.push(key);
    Ok(parts.join("+"))
}

fn portal_key_name(code: Code) -> Option<&'static str> {
    Some(match code {
        Code::KeyA => "a",
        Code::KeyB => "b",
        Code::KeyC => "c",
        Code::KeyD => "d",
        Code::KeyE => "e",
        Code::KeyF => "f",
        Code::KeyG => "g",
        Code::KeyH => "h",
        Code::KeyI => "i",
        Code::KeyJ => "j",
        Code::KeyK => "k",
        Code::KeyL => "l",
        Code::KeyM => "m",
        Code::KeyN => "n",
        Code::KeyO => "o",
        Code::KeyP => "p",
        Code::KeyQ => "q",
        Code::KeyR => "r",
        Code::KeyS => "s",
        Code::KeyT => "t",
        Code::KeyU => "u",
        Code::KeyV => "v",
        Code::KeyW => "w",
        Code::KeyX => "x",
        Code::KeyY => "y",
        Code::KeyZ => "z",
        Code::Digit0 => "0",
        Code::Digit1 => "1",
        Code::Digit2 => "2",
        Code::Digit3 => "3",
        Code::Digit4 => "4",
        Code::Digit5 => "5",
        Code::Digit6 => "6",
        Code::Digit7 => "7",
        Code::Digit8 => "8",
        Code::Digit9 => "9",
        Code::Backquote => "grave",
        Code::Backslash => "backslash",
        Code::BracketLeft => "bracketleft",
        Code::BracketRight => "bracketright",
        Code::Comma => "comma",
        Code::Equal => "equal",
        Code::Minus => "minus",
        Code::Period => "period",
        Code::Quote => "apostrophe",
        Code::Semicolon => "semicolon",
        Code::Slash => "slash",
        Code::Backspace => "BackSpace",
        Code::CapsLock => "Caps_Lock",
        Code::Enter => "Return",
        Code::Space => "space",
        Code::Tab => "Tab",
        Code::Delete => "Delete",
        Code::End => "End",
        Code::Home => "Home",
        Code::Insert => "Insert",
        Code::PageDown => "Page_Down",
        Code::PageUp => "Page_Up",
        Code::ArrowDown => "Down",
        Code::ArrowLeft => "Left",
        Code::ArrowRight => "Right",
        Code::ArrowUp => "Up",
        Code::PrintScreen => "Print",
        Code::ScrollLock => "Scroll_Lock",
        Code::Pause => "Pause",
        Code::Numpad0 => "KP_0",
        Code::Numpad1 => "KP_1",
        Code::Numpad2 => "KP_2",
        Code::Numpad3 => "KP_3",
        Code::Numpad4 => "KP_4",
        Code::Numpad5 => "KP_5",
        Code::Numpad6 => "KP_6",
        Code::Numpad7 => "KP_7",
        Code::Numpad8 => "KP_8",
        Code::Numpad9 => "KP_9",
        Code::NumpadAdd => "KP_Add",
        Code::NumpadDecimal => "KP_Decimal",
        Code::NumpadDivide => "KP_Divide",
        Code::NumpadMultiply => "KP_Multiply",
        Code::NumpadSubtract => "KP_Subtract",
        Code::F1 => "F1",
        Code::F2 => "F2",
        Code::F3 => "F3",
        Code::F4 => "F4",
        Code::F5 => "F5",
        Code::F6 => "F6",
        Code::F7 => "F7",
        Code::F8 => "F8",
        Code::F9 => "F9",
        Code::F10 => "F10",
        Code::F11 => "F11",
        Code::F12 => "F12",
        Code::F13 => "F13",
        Code::F14 => "F14",
        Code::F15 => "F15",
        Code::F16 => "F16",
        Code::F17 => "F17",
        Code::F18 => "F18",
        Code::F19 => "F19",
        Code::F20 => "F20",
        Code::F21 => "F21",
        Code::F22 => "F22",
        Code::F23 => "F23",
        Code::F24 => "F24",
        _ => return None,
    })
}
