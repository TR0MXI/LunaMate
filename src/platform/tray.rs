//! 创建系统托盘，并把原生菜单事件转换为有界应用动作。

use async_channel::{Receiver, Sender, TrySendError};
use rust_i18n::t;
use tokio::runtime::Handle;

const ACTION_CHANNEL_CAPACITY: usize = 16;
const TRAY_ICON_SIZE: u32 = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SystemTrayAction {
    ToggleDesktopPet,
    OpenSettings,
    Quit,
}

struct TrayLabels {
    hide_desktop_pet: String,
    settings: String,
    quit: String,
}

impl TrayLabels {
    fn localized() -> Self {
        Self {
            hide_desktop_pet: t!("tray.hide_desktop_pet").to_string(),
            settings: t!("tray.settings").to_string(),
            quit: t!("tray.quit").to_string(),
        }
    }
}

/// 持有平台托盘资源，直到 GPUI 事件任务随应用退出而释放。
pub(crate) struct SystemTray {
    inner: imp::PlatformTray,
}

impl SystemTray {
    /// 创建托盘并返回菜单动作接收端。
    pub(crate) fn install(runtime: &Handle) -> Result<(Self, Receiver<SystemTrayAction>), String> {
        let (actions, receiver) = async_channel::bounded(ACTION_CHANNEL_CAPACITY);
        let inner = imp::PlatformTray::new(actions, runtime, TrayLabels::localized())?;
        Ok((Self { inner }, receiver))
    }

    /// 将桌宠的实际显隐状态同步回可勾选菜单项。
    pub(crate) fn set_desktop_pet_hidden(&self, hidden: bool) {
        self.inner.set_desktop_pet_hidden(hidden);
    }
}

fn dispatch_action(actions: &Sender<SystemTrayAction>, action: SystemTrayAction) {
    match actions.try_send(action) {
        Ok(()) | Err(TrySendError::Closed(_)) => {}
        Err(TrySendError::Full(_)) => log::warn!("{}", t!("log.tray_action_queue_full")),
    }
}

pub(in crate::platform) fn tray_icon_rgba() -> Vec<u8> {
    let mut pixels = vec![0; (TRAY_ICON_SIZE * TRAY_ICON_SIZE * 4) as usize];
    for y in 0..TRAY_ICON_SIZE {
        for x in 0..TRAY_ICON_SIZE {
            let sample_x = x as f32 + 0.5;
            let sample_y = y as f32 + 0.5;
            let moon = circle_coverage(sample_x, sample_y, 14.0, 16.0, 11.5)
                * (1.0 - circle_coverage(sample_x, sample_y, 18.5, 12.5, 10.0));
            let star = circle_coverage(sample_x, sample_y, 23.0, 23.0, 2.0);
            let (color, alpha) = if star > moon {
                ([255, 211, 94], star)
            } else {
                ([126, 102, 255], moon)
            };
            let offset = ((y * TRAY_ICON_SIZE + x) * 4) as usize;
            pixels[offset..offset + 3].copy_from_slice(&color);
            pixels[offset + 3] = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    pixels
}

fn circle_coverage(x: f32, y: f32, center_x: f32, center_y: f32, radius: f32) -> f32 {
    let distance = (x - center_x).hypot(y - center_y);
    (radius + 0.5 - distance).clamp(0.0, 1.0)
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
mod imp {
    use async_channel::Sender;
    use tokio::runtime::Handle;
    use tray_icon::{
        Icon, TrayIcon, TrayIconBuilder,
        menu::{CheckMenuItem, Menu, MenuEvent, MenuItem},
    };

    use super::{SystemTrayAction, TRAY_ICON_SIZE, TrayLabels, dispatch_action, tray_icon_rgba};

    const HIDE_DESKTOP_PET_ID: &str = "lunamate-hide-desktop-pet";
    const SETTINGS_ID: &str = "lunamate-settings";
    const QUIT_ID: &str = "lunamate-quit";

    pub(super) struct PlatformTray {
        _tray_icon: TrayIcon,
        hide_desktop_pet: CheckMenuItem,
    }

    impl PlatformTray {
        pub(super) fn new(
            actions: Sender<SystemTrayAction>,
            _runtime: &Handle,
            labels: TrayLabels,
        ) -> Result<Self, String> {
            let hide_desktop_pet = CheckMenuItem::with_id(
                HIDE_DESKTOP_PET_ID,
                labels.hide_desktop_pet,
                true,
                false,
                None,
            );
            let settings = MenuItem::with_id(SETTINGS_ID, labels.settings, true, None);
            let quit = MenuItem::with_id(QUIT_ID, labels.quit, true, None);
            let menu = Menu::new();
            menu.append_items(&[&hide_desktop_pet, &settings, &quit])
                .map_err(|error| format!("无法创建系统托盘菜单：{error}"))?;

            let icon = Icon::from_rgba(tray_icon_rgba(), TRAY_ICON_SIZE, TRAY_ICON_SIZE)
                .map_err(|error| format!("无法创建系统托盘图标：{error}"))?;
            let tray_icon = TrayIconBuilder::new()
                .with_tooltip("LunaMate")
                .with_icon(icon)
                .with_icon_as_template(cfg!(target_os = "macos"))
                .with_menu(Box::new(menu))
                .build()
                .map_err(|error| format!("无法创建系统托盘：{error}"))?;

            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                let action = match event.id.as_ref() {
                    HIDE_DESKTOP_PET_ID => Some(SystemTrayAction::ToggleDesktopPet),
                    SETTINGS_ID => Some(SystemTrayAction::OpenSettings),
                    QUIT_ID => Some(SystemTrayAction::Quit),
                    _ => None,
                };
                if let Some(action) = action {
                    dispatch_action(&actions, action);
                }
            }));

            Ok(Self {
                _tray_icon: tray_icon,
                hide_desktop_pet,
            })
        }

        pub(super) fn set_desktop_pet_hidden(&self, hidden: bool) {
            self.hide_desktop_pet.set_checked(hidden);
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use async_channel::Sender;
    use ksni::{TrayMethods, menu::CheckmarkItem, menu::StandardItem};
    use rust_i18n::t;
    use tokio::runtime::Handle;

    use super::{SystemTrayAction, TRAY_ICON_SIZE, TrayLabels, dispatch_action, tray_icon_rgba};

    struct LinuxTray {
        actions: Sender<SystemTrayAction>,
        refresh: Sender<()>,
        hidden: Arc<AtomicBool>,
        labels: TrayLabels,
        icon: ksni::Icon,
    }

    impl LinuxTray {
        fn toggle_desktop_pet(&self) {
            let hidden = !self.hidden.load(Ordering::Acquire);
            self.hidden.store(hidden, Ordering::Release);
            let _ = self.refresh.try_send(());
            dispatch_action(&self.actions, SystemTrayAction::ToggleDesktopPet);
        }
    }

    impl ksni::Tray for LinuxTray {
        const MENU_ON_ACTIVATE: bool = true;

        fn id(&self) -> String {
            "lunamate".to_owned()
        }

        fn title(&self) -> String {
            "LunaMate".to_owned()
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            vec![self.icon.clone()]
        }

        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            vec![
                CheckmarkItem {
                    label: self.labels.hide_desktop_pet.clone(),
                    checked: self.hidden.load(Ordering::Acquire),
                    activate: Box::new(|tray: &mut Self| tray.toggle_desktop_pet()),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: self.labels.settings.clone(),
                    activate: Box::new(|tray: &mut Self| {
                        dispatch_action(&tray.actions, SystemTrayAction::OpenSettings);
                    }),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: self.labels.quit.clone(),
                    activate: Box::new(|tray: &mut Self| {
                        dispatch_action(&tray.actions, SystemTrayAction::Quit);
                    }),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }

    pub(super) struct PlatformTray {
        hidden: Arc<AtomicBool>,
        refresh: Sender<()>,
        shutdown: Sender<()>,
        _task: tokio::task::JoinHandle<()>,
    }

    impl PlatformTray {
        pub(super) fn new(
            actions: Sender<SystemTrayAction>,
            runtime: &Handle,
            labels: TrayLabels,
        ) -> Result<Self, String> {
            let hidden = Arc::new(AtomicBool::new(false));
            let (refresh, refresh_receiver) = async_channel::bounded(1);
            let (shutdown, shutdown_receiver) = async_channel::bounded(1);
            let tray = LinuxTray {
                actions,
                refresh: refresh.clone(),
                hidden: hidden.clone(),
                labels,
                icon: linux_icon(),
            };
            let task = runtime.spawn(async move {
                let handle = match tray.assume_sni_available(true).spawn().await {
                    Ok(handle) => handle,
                    Err(error) => {
                        log::warn!("{}", t!("log.tray_init_failed", error = error.to_string()));
                        return;
                    }
                };

                loop {
                    tokio::select! {
                        _ = shutdown_receiver.recv() => break,
                        refreshed = refresh_receiver.recv() => {
                            if refreshed.is_err() {
                                break;
                            }
                            while refresh_receiver.try_recv().is_ok() {}
                            let _ = handle.update(|_: &mut LinuxTray| {}).await;
                        }
                    }
                }
                handle.shutdown().await;
            });

            Ok(Self {
                hidden,
                refresh,
                shutdown,
                _task: task,
            })
        }

        pub(super) fn set_desktop_pet_hidden(&self, hidden: bool) {
            self.hidden.store(hidden, Ordering::Release);
            let _ = self.refresh.try_send(());
        }
    }

    impl Drop for PlatformTray {
        fn drop(&mut self) {
            let _ = self.shutdown.try_send(());
        }
    }

    fn linux_icon() -> ksni::Icon {
        let mut data = tray_icon_rgba();
        for pixel in data.chunks_exact_mut(4) {
            pixel.rotate_right(1);
        }
        ksni::Icon {
            width: TRAY_ICON_SIZE as i32,
            height: TRAY_ICON_SIZE as i32,
            data,
        }
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
mod imp {
    use async_channel::Sender;
    use tokio::runtime::Handle;

    use super::{SystemTrayAction, TrayLabels};

    pub(super) struct PlatformTray;

    impl PlatformTray {
        pub(super) fn new(
            _actions: Sender<SystemTrayAction>,
            _runtime: &Handle,
            _labels: TrayLabels,
        ) -> Result<Self, String> {
            Err("当前平台不支持系统托盘".to_owned())
        }

        pub(super) fn set_desktop_pet_hidden(&self, _hidden: bool) {}
    }
}
