//! 建立 portal session，并驱动绑定、边沿事件、健康检查与关闭流程。

use ashpd::{
    AppID,
    desktop::{
        CreateSessionOptions, ResponseError, Session,
        global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, ListShortcutsOptions},
    },
};
use async_channel::Sender;
use futures::StreamExt as _;
use global_hotkey::HotKeyState;
use tokio::sync::watch;

use crate::{config::ShortcutAction, logging::sanitize_log_field, platform::APPLICATION_ID};

use super::{
    PORTAL_HEALTH_INTERVAL, PORTAL_HEALTH_TIMEOUT, PortalConnectionExit, PortalState,
    RECONFIGURE_DELAY, ShortcutEvent,
    bindings::{
        ActiveBindings, bound_actions, missing_binding_errors, requested_shortcuts,
        runtime_bindings,
    },
    send_runtime_bindings, send_runtime_errors, send_shortcut_event,
};

pub(super) async fn run_portal_connection(
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
            log::warn!("event=portal_host_registry_unavailable fallback=portal_app_id");
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
                    "event=portal_shortcuts_restored count={}",
                    response.shortcuts().len()
                ),
                Err(_) => log::warn!("event=portal_shortcut_list_failed stage=response"),
            },
            Err(_) => log::warn!("event=portal_shortcut_list_failed stage=request"),
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
                        log::warn!(
                            "event=portal_unknown_shortcut shortcut_id={}",
                            sanitize_log_field(event.shortcut_id())
                        );
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
