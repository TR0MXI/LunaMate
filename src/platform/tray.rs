//! 创建系统托盘，并把图标点击与原生菜单事件转换为有界应用动作。

use async_channel::{Receiver, Sender, TrySendError};
use rust_i18n::t;
use tokio::runtime::Handle;

const ACTION_CHANNEL_CAPACITY: usize = 16;
const TRAY_ICON_SIZE: u32 = 32;
const FALLBACK_TRAY_ICON_LOGICAL_SIZE: f32 = 24.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SystemTrayAction {
    ToggleDesktopPet,
    OpenSettings,
    #[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
    OpenMenu(TrayMenuAnchor),
    Quit,
}

/// 描述自定义托盘菜单使用的逻辑屏幕坐标锚点。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TrayMenuAnchor {
    pub(crate) icon_origin: [f32; 2],
    pub(crate) icon_size: [f32; 2],
    pub(crate) scale_factor: f64,
}

impl TrayMenuAnchor {
    #[cfg_attr(
        not(any(target_os = "windows", target_os = "macos", test)),
        allow(dead_code)
    )]
    pub(in crate::platform) fn from_physical(
        click_position: [f64; 2],
        icon_position: [f64; 2],
        size: [u32; 2],
        scale_factor: f64,
    ) -> Self {
        let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
            scale_factor
        } else {
            1.0
        };
        let logical_coordinate = |value: f64| {
            let value = value / scale_factor;
            if value.is_finite() {
                value.clamp(f64::from(f32::MIN), f64::from(f32::MAX)) as f32
            } else {
                0.0
            }
        };
        let logical_size = |value: u32| {
            let value = logical_coordinate(f64::from(value));
            if value > 0.0 {
                value
            } else {
                FALLBACK_TRAY_ICON_LOGICAL_SIZE
            }
        };
        let click_position = [
            logical_coordinate(click_position[0]),
            logical_coordinate(click_position[1]),
        ];
        let reported_origin = [
            logical_coordinate(icon_position[0]),
            logical_coordinate(icon_position[1]),
        ];
        let icon_size = [logical_size(size[0]), logical_size(size[1])];
        let click_inside_reported_icon = (reported_origin[0]..=reported_origin[0] + icon_size[0])
            .contains(&click_position[0])
            && (reported_origin[1]..=reported_origin[1] + icon_size[1])
                .contains(&click_position[1]);
        let icon_origin = if click_inside_reported_icon {
            reported_origin
        } else {
            [
                click_position[0] - icon_size[0] / 2.0,
                click_position[1] - icon_size[1] / 2.0,
            ]
        };
        Self {
            icon_origin,
            icon_size,
            scale_factor,
        }
    }
}

#[derive(Clone)]
struct TrayLabels {
    hide_desktop_pet: String,
    settings: String,
    quit: String,
}

/// 描述托盘位图使用的两种语义色；原生菜单本身仍由桌面环境绘制。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TrayIconStyle {
    moon: [u8; 3],
    star: [u8; 3],
}

impl TrayIconStyle {
    pub(crate) fn new(moon: [u8; 3], star: [u8; 3]) -> Self {
        Self { moon, star }
    }
}

impl Default for TrayIconStyle {
    fn default() -> Self {
        Self::new([126, 102, 255], [255, 211, 94])
    }
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
    actions: Sender<SystemTrayAction>,
}

impl SystemTray {
    /// 创建托盘并返回菜单动作接收端。
    pub(crate) fn install(
        runtime: &Handle,
        use_native_menu: bool,
    ) -> Result<(Self, Receiver<SystemTrayAction>), String> {
        let (actions, receiver) = async_channel::bounded(ACTION_CHANNEL_CAPACITY);
        let inner = imp::PlatformTray::new(
            actions.clone(),
            runtime,
            TrayLabels::localized(),
            use_native_menu,
        )?;
        Ok((Self { inner, actions }, receiver))
    }

    /// 将桌宠的实际显隐状态同步回可勾选菜单项。
    pub(crate) fn set_desktop_pet_hidden(&self, hidden: bool) {
        self.inner.set_desktop_pet_hidden(hidden);
    }

    /// 同步当前 GPUI 语义色，并在语言切换后刷新原生菜单文本。
    pub(crate) fn sync_appearance(&self, style: TrayIconStyle) -> Result<(), String> {
        self.inner.sync_appearance(style, TrayLabels::localized())
    }

    /// 即时切换右键是否由系统原生菜单处理。
    pub(crate) fn set_use_native_menu(&self, enabled: bool) {
        self.inner.set_use_native_menu(enabled);
    }

    /// 是否可以在原生菜单与自绘菜单之间切换；否则设置项不应展示给用户。
    pub(crate) const fn supports_menu_style_choice() -> bool {
        imp::SUPPORTS_MENU_STYLE_CHOICE
    }

    /// 返回当前右键是否应由原生菜单处理；不支持自绘的平台恒为 `true`。
    pub(crate) fn uses_native_menu(&self) -> bool {
        self.inner.uses_native_menu()
    }

    /// 自定义窗口无法创建时，在当前光标位置显示保留的原生菜单。
    pub(crate) fn show_native_menu(&self) {
        self.inner.show_native_menu();
    }

    /// 将自定义菜单项重新汇入与原生菜单相同的有界动作队列。
    pub(crate) fn request_action(&self, action: SystemTrayAction) {
        dispatch_action(&self.actions, action);
    }
}

fn dispatch_action(actions: &Sender<SystemTrayAction>, action: SystemTrayAction) {
    match actions.try_send(action) {
        Ok(()) | Err(TrySendError::Closed(_)) => {}
        Err(TrySendError::Full(_)) => log::warn!("{}", t!("log.tray_action_queue_full")),
    }
}

pub(in crate::platform) fn tray_icon_rgba(style: TrayIconStyle) -> Vec<u8> {
    let mut pixels = vec![0; (TRAY_ICON_SIZE * TRAY_ICON_SIZE * 4) as usize];
    for y in 0..TRAY_ICON_SIZE {
        for x in 0..TRAY_ICON_SIZE {
            let sample_x = x as f32 + 0.5;
            let sample_y = y as f32 + 0.5;
            let moon = circle_coverage(sample_x, sample_y, 14.0, 16.0, 11.5)
                * (1.0 - circle_coverage(sample_x, sample_y, 18.5, 12.5, 10.0));
            let star = circle_coverage(sample_x, sample_y, 23.0, 23.0, 2.0);
            let (color, alpha) = if star > moon {
                (style.star, star)
            } else {
                (style.moon, moon)
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
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use async_channel::Sender;
    use tokio::runtime::Handle;
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
        menu::{CheckMenuItem, Menu, MenuEvent, MenuItem},
    };

    use super::{
        SystemTrayAction, TRAY_ICON_SIZE, TrayIconStyle, TrayLabels, TrayMenuAnchor,
        dispatch_action, tray_icon_rgba,
    };

    const HIDE_DESKTOP_PET_ID: &str = "lunamate-hide-desktop-pet";
    const SETTINGS_ID: &str = "lunamate-settings";
    const QUIT_ID: &str = "lunamate-quit";

    pub(super) const SUPPORTS_MENU_STYLE_CHOICE: bool = true;

    pub(super) struct PlatformTray {
        tray_icon: TrayIcon,
        hide_desktop_pet: CheckMenuItem,
        settings: MenuItem,
        quit: MenuItem,
        use_native_menu: Arc<AtomicBool>,
    }

    impl PlatformTray {
        pub(super) fn new(
            actions: Sender<SystemTrayAction>,
            _runtime: &Handle,
            labels: TrayLabels,
            use_native_menu: bool,
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

            let icon = Icon::from_rgba(
                tray_icon_rgba(TrayIconStyle::default()),
                TRAY_ICON_SIZE,
                TRAY_ICON_SIZE,
            )
            .map_err(|error| format!("无法创建系统托盘图标：{error}"))?;
            let tray_icon = TrayIconBuilder::new()
                .with_tooltip("LunaMate")
                .with_icon(icon)
                .with_icon_as_template(cfg!(target_os = "macos"))
                .with_menu(Box::new(menu))
                .with_menu_on_right_click(use_native_menu)
                .build()
                .map_err(|error| format!("无法创建系统托盘：{error}"))?;
            let tray_id = tray_icon.id().clone();
            let native_menu = Arc::new(AtomicBool::new(use_native_menu));

            let actions_for_menu = actions.clone();
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                let action = match event.id.as_ref() {
                    HIDE_DESKTOP_PET_ID => Some(SystemTrayAction::ToggleDesktopPet),
                    SETTINGS_ID => Some(SystemTrayAction::OpenSettings),
                    QUIT_ID => Some(SystemTrayAction::Quit),
                    _ => None,
                };
                if let Some(action) = action {
                    dispatch_action(&actions_for_menu, action);
                }
            }));
            let actions_for_click = actions;
            let native_menu_for_click = native_menu.clone();
            TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                let TrayIconEvent::Click {
                    id,
                    position,
                    rect,
                    button: MouseButton::Right,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                else {
                    return;
                };
                if id != tray_id || native_menu_for_click.load(Ordering::Acquire) {
                    return;
                }
                let scale_factor = tray_scale_factor(position.x, position.y);
                let anchor = TrayMenuAnchor::from_physical(
                    [position.x, position.y],
                    [rect.position.x, rect.position.y],
                    [rect.size.width, rect.size.height],
                    scale_factor,
                );
                dispatch_action(&actions_for_click, SystemTrayAction::OpenMenu(anchor));
            }));

            Ok(Self {
                tray_icon,
                hide_desktop_pet,
                settings,
                quit,
                use_native_menu: native_menu,
            })
        }

        pub(super) fn set_desktop_pet_hidden(&self, hidden: bool) {
            self.hide_desktop_pet.set_checked(hidden);
        }

        pub(super) fn sync_appearance(
            &self,
            style: TrayIconStyle,
            labels: TrayLabels,
        ) -> Result<(), String> {
            self.hide_desktop_pet.set_text(labels.hide_desktop_pet);
            self.settings.set_text(labels.settings);
            self.quit.set_text(labels.quit);
            let icon = Icon::from_rgba(tray_icon_rgba(style), TRAY_ICON_SIZE, TRAY_ICON_SIZE)
                .map_err(|error| format!("无法生成主题托盘图标：{error}"))?;
            #[cfg(target_os = "macos")]
            self.tray_icon
                .set_icon_with_as_template(Some(icon), true)
                .map_err(|error| format!("无法更新主题托盘图标：{error}"))?;
            #[cfg(target_os = "windows")]
            self.tray_icon
                .set_icon(Some(icon))
                .map_err(|error| format!("无法更新主题托盘图标：{error}"))?;
            Ok(())
        }

        pub(super) fn set_use_native_menu(&self, enabled: bool) {
            self.use_native_menu.store(enabled, Ordering::Release);
            self.tray_icon.set_show_menu_on_right_click(enabled);
        }

        pub(super) fn uses_native_menu(&self) -> bool {
            self.use_native_menu.load(Ordering::Acquire)
        }

        pub(super) fn show_native_menu(&self) {
            self.tray_icon.show_menu();
        }
    }

    #[cfg(target_os = "windows")]
    fn tray_scale_factor(x: f64, y: f64) -> f64 {
        use windows::Win32::{
            Foundation::POINT,
            Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromPoint},
            UI::{
                HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
                WindowsAndMessaging::USER_DEFAULT_SCREEN_DPI,
            },
        };

        let point = POINT {
            x: x.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32,
            y: y.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32,
        };
        // SAFETY: `point` 是不含指针的已初始化屏幕坐标，调用只按值读取。
        let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };
        let mut dpi_x = USER_DEFAULT_SCREEN_DPI;
        let mut dpi_y = USER_DEFAULT_SCREEN_DPI;
        if monitor.is_invalid()
            // SAFETY: monitor 来自上一步系统查询；两个输出指针独占指向有效的局部 `u32`。
            || unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }
                .is_err()
        {
            return 1.0;
        }
        f64::from(dpi_x) / f64::from(USER_DEFAULT_SCREEN_DPI)
    }

    #[cfg(target_os = "macos")]
    fn tray_scale_factor(_x: f64, _y: f64) -> f64 {
        use cocoa::{appkit::NSScreen, base::nil};

        // SAFETY: `tray-icon` 在 AppKit 主线程同步发送点击事件；返回的 NSScreen 只在
        // 当前调用中读取，不保存 Objective-C 对象指针。
        unsafe {
            let screen = NSScreen::mainScreen(nil);
            if screen == nil {
                1.0
            } else {
                NSScreen::backingScaleFactor(screen)
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use async_channel::{Sender, TrySendError};
    use ksni::{TrayMethods, menu::CheckmarkItem, menu::StandardItem};
    use parking_lot::Mutex;
    use rust_i18n::t;
    use tokio::runtime::Handle;

    use super::{
        SystemTrayAction, TRAY_ICON_SIZE, TrayIconStyle, TrayLabels, dispatch_action,
        tray_icon_rgba,
    };

    /// StatusNotifierItem 的菜单完全由宿主绘制，应用无法接管右键并自绘。
    pub(super) const SUPPORTS_MENU_STYLE_CHOICE: bool = false;

    struct TrayPresentation {
        labels: TrayLabels,
        icon: ksni::Icon,
    }

    struct LinuxTray {
        actions: Sender<SystemTrayAction>,
        refresh: Sender<()>,
        hidden: Arc<AtomicBool>,
        presentation: Arc<Mutex<TrayPresentation>>,
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
            vec![self.presentation.lock().icon.clone()]
        }

        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            let labels = self.presentation.lock().labels.clone();
            vec![
                CheckmarkItem {
                    label: labels.hide_desktop_pet,
                    checked: self.hidden.load(Ordering::Acquire),
                    activate: Box::new(|tray: &mut Self| tray.toggle_desktop_pet()),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: labels.settings,
                    activate: Box::new(|tray: &mut Self| {
                        dispatch_action(&tray.actions, SystemTrayAction::OpenSettings);
                    }),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: labels.quit,
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
        presentation: Arc<Mutex<TrayPresentation>>,
        refresh: Sender<()>,
        shutdown: Sender<()>,
        _task: tokio::task::JoinHandle<()>,
    }

    impl PlatformTray {
        pub(super) fn new(
            actions: Sender<SystemTrayAction>,
            runtime: &Handle,
            labels: TrayLabels,
            _use_native_menu: bool,
        ) -> Result<Self, String> {
            let hidden = Arc::new(AtomicBool::new(false));
            let (refresh, refresh_receiver) = async_channel::bounded(1);
            let (shutdown, shutdown_receiver) = async_channel::bounded(1);
            let presentation = Arc::new(Mutex::new(TrayPresentation {
                labels,
                icon: linux_icon(TrayIconStyle::default()),
            }));
            let tray = LinuxTray {
                actions,
                refresh: refresh.clone(),
                hidden: hidden.clone(),
                presentation: presentation.clone(),
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
                            if handle.update(|_: &mut LinuxTray| {}).await.is_none() {
                                log::warn!("{}", t!("log.tray_update_stopped"));
                                break;
                            }
                        }
                    }
                }
                handle.shutdown().await;
            });

            Ok(Self {
                hidden,
                presentation,
                refresh,
                shutdown,
                _task: task,
            })
        }

        pub(super) fn set_desktop_pet_hidden(&self, hidden: bool) {
            self.hidden.store(hidden, Ordering::Release);
            let _ = self.refresh.try_send(());
        }

        pub(super) fn sync_appearance(
            &self,
            style: TrayIconStyle,
            labels: TrayLabels,
        ) -> Result<(), String> {
            *self.presentation.lock() = TrayPresentation {
                labels,
                icon: linux_icon(style),
            };
            match self.refresh.try_send(()) {
                Ok(()) | Err(TrySendError::Full(())) => Ok(()),
                Err(TrySendError::Closed(())) => Err("系统托盘后台任务已结束".to_owned()),
            }
        }

        pub(super) fn set_use_native_menu(&self, _enabled: bool) {}

        pub(super) fn uses_native_menu(&self) -> bool {
            true
        }

        pub(super) fn show_native_menu(&self) {}
    }

    impl Drop for PlatformTray {
        fn drop(&mut self) {
            let _ = self.shutdown.try_send(());
        }
    }

    fn linux_icon(style: TrayIconStyle) -> ksni::Icon {
        let mut data = tray_icon_rgba(style);
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

    use super::{SystemTrayAction, TrayIconStyle, TrayLabels};

    pub(super) const SUPPORTS_MENU_STYLE_CHOICE: bool = false;

    pub(super) struct PlatformTray;

    impl PlatformTray {
        pub(super) fn new(
            _actions: Sender<SystemTrayAction>,
            _runtime: &Handle,
            _labels: TrayLabels,
            _use_native_menu: bool,
        ) -> Result<Self, String> {
            Err("当前平台不支持系统托盘".to_owned())
        }

        pub(super) fn set_desktop_pet_hidden(&self, _hidden: bool) {}

        pub(super) fn sync_appearance(
            &self,
            _style: TrayIconStyle,
            _labels: TrayLabels,
        ) -> Result<(), String> {
            Ok(())
        }

        pub(super) fn set_use_native_menu(&self, _enabled: bool) {}

        pub(super) fn uses_native_menu(&self) -> bool {
            true
        }

        pub(super) fn show_native_menu(&self) {}
    }
}
