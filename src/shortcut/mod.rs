//! 封装系统全局快捷键注册、事件路由和 GPUI 按键录入转换。

#[cfg(target_os = "linux")]
mod wayland;

#[cfg(test)]
mod tests;

#[cfg(not(target_os = "linux"))]
use std::sync::LazyLock;

use async_channel::Receiver;
#[cfg(not(target_os = "linux"))]
use async_channel::Sender;
#[cfg(not(target_os = "linux"))]
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
#[cfg(test)]
use gpui::Keystroke;
use gpui::{KeybindingKeystroke, Window};
use keyboard_types::{Code, Modifiers};

use crate::config::{KeyboardShortcut, ShortcutAction, ShortcutSettings};
#[cfg(target_os = "linux")]
use crate::platform::wayland_activation_target;

/// 已注册快捷键产生的低频按下或松开事件。
#[derive(Debug)]
pub(crate) enum ShortcutEvent {
    #[cfg(not(target_os = "linux"))]
    Native {
        revision: u64,
        id: u32,
        state: ShortcutState,
    },
    #[cfg(target_os = "linux")]
    Portal {
        revision: u64,
        action: ShortcutAction,
        state: ShortcutState,
        activation_token: Option<String>,
    },
    #[cfg(target_os = "linux")]
    RuntimeErrors { revision: u64, errors: Vec<String> },
    #[cfg(target_os = "linux")]
    RuntimeBindings {
        revision: u64,
        bindings: Vec<ShortcutRuntimeBinding>,
    },
}

/// Wayland 合成器确认的动作与人类可读触发方式。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ShortcutRuntimeBinding {
    action: ShortcutAction,
    trigger_description: String,
}

impl ShortcutRuntimeBinding {
    #[cfg(target_os = "linux")]
    pub(crate) fn new(action: ShortcutAction, trigger_description: String) -> Self {
        Self {
            action,
            trigger_description,
        }
    }

    pub(crate) const fn action(&self) -> ShortcutAction {
        self.action
    }

    pub(crate) fn trigger_description(&self) -> &str {
        &self.trigger_description
    }
}

#[cfg(not(target_os = "linux"))]
static SHORTCUT_EVENTS: LazyLock<(Sender<ShortcutEvent>, Receiver<ShortcutEvent>)> =
    LazyLock::new(|| {
        // global-hotkey 只发送离散按下/松开边沿；无界通道确保松开事件不会因瞬时 UI
        // 阻塞丢失，语音按住仍有独立超时兜底。
        let (sender, events) = async_channel::unbounded();
        let handler_sender = sender.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            let revision = *NATIVE_EVENT_REVISION.lock();
            let _ = handler_sender.try_send(ShortcutEvent::Native {
                revision,
                id: event.id,
                state: match event.state {
                    HotKeyState::Pressed => ShortcutState::Pressed,
                    HotKeyState::Released => ShortcutState::Released,
                },
            });
        }));
        (sender, events)
    });

#[cfg(not(target_os = "linux"))]
static NATIVE_EVENT_REVISION: LazyLock<parking_lot::Mutex<u64>> =
    LazyLock::new(|| parking_lot::Mutex::new(0));

/// 已解析到应用动作的快捷键按下或松开事件。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedShortcutEvent {
    action: ShortcutAction,
    state: ShortcutState,
    activation_token: Option<String>,
}

impl ResolvedShortcutEvent {
    pub(crate) const fn action(&self) -> ShortcutAction {
        self.action
    }

    pub(crate) const fn is_pressed(&self) -> bool {
        matches!(self.state, ShortcutState::Pressed)
    }

    pub(crate) fn activation_token(&self) -> Option<&str> {
        self.activation_token.as_deref()
    }
}

/// 统一原生注册器和 Wayland portal 的按键边沿。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShortcutState {
    Pressed,
    Released,
}

#[cfg(not(target_os = "linux"))]
struct NativeShortcutManager {
    manager: GlobalHotKeyManager,
    registered: Vec<(ShortcutAction, HotKey)>,
}

enum ShortcutBackend {
    #[cfg(not(target_os = "linux"))]
    Native(NativeShortcutManager),
    #[cfg(target_os = "linux")]
    Wayland(wayland::WaylandShortcutManager),
}

/// 在 UI 线程持有原生 manager，或控制异步 Wayland portal session。
pub(crate) struct ShortcutManager {
    backend: ShortcutBackend,
    settings: ShortcutSettings,
    events: Receiver<ShortcutEvent>,
    suspended: bool,
    #[cfg(not(target_os = "linux"))]
    native_revision: u64,
}

impl ShortcutManager {
    /// 在原生事件循环所在的 UI 线程创建 manager 并注册启动配置。
    pub(crate) fn new(
        settings: ShortcutSettings,
        window: &Window,
        runtime: &tokio::runtime::Handle,
    ) -> Result<(Self, Vec<String>), String> {
        #[cfg(target_os = "linux")]
        {
            let target = wayland_activation_target(window)?;
            let (manager, events) =
                wayland::WaylandShortcutManager::new(settings.clone(), target, runtime);
            Ok((
                Self {
                    backend: ShortcutBackend::Wayland(manager),
                    settings,
                    events,
                    suspended: false,
                    #[cfg(not(target_os = "linux"))]
                    native_revision: 0,
                },
                Vec::new(),
            ))
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = runtime;
            ensure_native_platform_supported(window)?;
            let manager = GlobalHotKeyManager::new()
                .map_err(|error| format!("无法创建全局快捷键管理器：{error}"))?;
            let events = SHORTCUT_EVENTS.1.clone();
            let mut this = Self {
                backend: ShortcutBackend::Native(NativeShortcutManager {
                    manager,
                    registered: Vec::new(),
                }),
                settings: ShortcutSettings::default(),
                events,
                suspended: false,
                #[cfg(not(target_os = "linux"))]
                native_revision: 0,
            };
            let errors = this.configure(settings);
            Ok((this, errors))
        }
    }

    pub(crate) fn events(&self) -> Receiver<ShortcutEvent> {
        self.events.clone()
    }

    /// 用完整快照替换全部注册；单个系统冲突不会阻止其他动作生效。
    pub(crate) fn configure(&mut self, settings: ShortcutSettings) -> Vec<String> {
        let errors = match &mut self.backend {
            #[cfg(not(target_os = "linux"))]
            ShortcutBackend::Native(manager) => manager.configure(&settings, self.suspended),
            #[cfg(target_os = "linux")]
            ShortcutBackend::Wayland(manager) => {
                manager.configure(settings.clone());
                Vec::new()
            }
        };
        self.settings = settings;
        #[cfg(not(target_os = "linux"))]
        if matches!(&self.backend, ShortcutBackend::Native(_)) {
            self.advance_native_revision();
        }
        errors
    }

    /// 录入期间释放系统按键占用，结束后恢复当前配置。
    pub(crate) fn set_suspended(&mut self, suspended: bool) -> Vec<String> {
        if self.suspended == suspended {
            return Vec::new();
        }
        self.suspended = suspended;
        let errors = match &mut self.backend {
            #[cfg(not(target_os = "linux"))]
            ShortcutBackend::Native(manager) => manager.set_suspended(&self.settings, suspended),
            #[cfg(target_os = "linux")]
            ShortcutBackend::Wayland(manager) => {
                manager.set_suspended(suspended);
                Vec::new()
            }
        };
        #[cfg(not(target_os = "linux"))]
        if matches!(&self.backend, ShortcutBackend::Native(_)) {
            self.advance_native_revision();
        }
        errors
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn reports_status_asynchronously(&self) -> bool {
        matches!(&self.backend, ShortcutBackend::Wayland(_))
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) const fn reports_status_asynchronously(&self) -> bool {
        false
    }

    pub(crate) fn resolve(&self, event: &ShortcutEvent) -> Option<ResolvedShortcutEvent> {
        if self.suspended {
            return None;
        }
        match (&self.backend, event) {
            #[cfg(not(target_os = "linux"))]
            (
                ShortcutBackend::Native(manager),
                ShortcutEvent::Native {
                    revision,
                    id,
                    state,
                },
            ) => manager.registered.iter().find_map(|(action, hotkey)| {
                (*revision == self.native_revision
                    && hotkey.id() == *id
                    && self
                        .settings
                        .shortcut(*action)
                        .is_some_and(|shortcut| shortcut.hotkey() == *hotkey))
                .then_some(ResolvedShortcutEvent {
                    action: *action,
                    state: *state,
                    activation_token: None,
                })
            }),
            #[cfg(target_os = "linux")]
            (
                ShortcutBackend::Wayland(manager),
                ShortcutEvent::Portal {
                    revision,
                    action,
                    state,
                    activation_token,
                },
            ) if manager.current_revision() == *revision
                && self.settings.shortcut(*action).is_some() =>
            {
                Some(ResolvedShortcutEvent {
                    action: *action,
                    state: *state,
                    activation_token: activation_token.clone(),
                })
            }
            _ => None,
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn runtime_errors<'a>(&self, event: &'a ShortcutEvent) -> Option<&'a [String]> {
        match (&self.backend, event) {
            (
                ShortcutBackend::Wayland(manager),
                ShortcutEvent::RuntimeErrors { revision, errors },
            ) if manager.current_revision() == *revision => Some(errors),
            _ => None,
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn runtime_bindings<'a>(
        &self,
        event: &'a ShortcutEvent,
    ) -> Option<&'a [ShortcutRuntimeBinding]> {
        match (&self.backend, event) {
            (
                ShortcutBackend::Wayland(manager),
                ShortcutEvent::RuntimeBindings { revision, bindings },
            ) if manager.current_revision() == *revision => Some(bindings),
            _ => None,
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) const fn runtime_bindings<'a>(
        &self,
        _event: &'a ShortcutEvent,
    ) -> Option<&'a [ShortcutRuntimeBinding]> {
        None
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) const fn runtime_errors<'a>(
        &self,
        _event: &'a ShortcutEvent,
    ) -> Option<&'a [String]> {
        None
    }

    pub(crate) fn activate_wayland(&self, _token: String) -> Result<(), String> {
        match &self.backend {
            #[cfg(target_os = "linux")]
            ShortcutBackend::Wayland(manager) => manager.activate(_token),
            #[cfg(not(target_os = "linux"))]
            ShortcutBackend::Native(_) => Ok(()),
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn advance_native_revision(&mut self) {
        let mut revision = NATIVE_EVENT_REVISION.lock();
        while self.events.try_recv().is_ok() {}
        *revision = revision.wrapping_add(1).max(1);
        self.native_revision = *revision;
    }
}

#[cfg(not(target_os = "linux"))]
impl NativeShortcutManager {
    fn configure(&mut self, settings: &ShortcutSettings, suspended: bool) -> Vec<String> {
        let registered = std::mem::take(&mut self.registered);
        let mut removed = Vec::new();
        for (action, hotkey) in registered {
            if let Some(next_action) = ShortcutAction::ALL.into_iter().find(|candidate| {
                settings
                    .shortcut(*candidate)
                    .is_some_and(|shortcut| shortcut.hotkey() == hotkey)
            }) {
                self.registered.push((next_action, hotkey));
            } else {
                removed.push((action, hotkey));
            }
        }
        let mut errors = self.unregister_entries(removed);
        if !suspended {
            errors.extend(self.register_current(settings));
        }
        errors
    }

    fn set_suspended(&mut self, settings: &ShortcutSettings, suspended: bool) -> Vec<String> {
        if suspended {
            self.unregister_current()
        } else {
            self.register_current(settings)
        }
    }

    fn unregister_current(&mut self) -> Vec<String> {
        let registered = std::mem::take(&mut self.registered);
        self.unregister_entries(registered)
    }

    fn unregister_entries(&mut self, registered: Vec<(ShortcutAction, HotKey)>) -> Vec<String> {
        let mut errors = Vec::new();
        for (action, hotkey) in registered {
            if let Err(error) = self.manager.unregister(hotkey) {
                errors.push(format!("{} 注销失败：{error}", action.id()));
                self.registered.push((action, hotkey));
            }
        }
        errors
    }

    fn register_current(&mut self, settings: &ShortcutSettings) -> Vec<String> {
        let mut errors = Vec::new();
        for action in ShortcutAction::ALL {
            let Some(shortcut) = settings.shortcut(action) else {
                continue;
            };
            let hotkey = shortcut.hotkey();
            if let Some((registered_action, _)) = self
                .registered
                .iter_mut()
                .find(|(_, registered)| *registered == hotkey)
            {
                *registered_action = action;
                continue;
            }
            match self.manager.register(hotkey) {
                Ok(()) => self.registered.push((action, hotkey)),
                Err(error) => errors.push(format!(
                    "{} ({}) 注册失败：{error}",
                    action.id(),
                    shortcut.id()
                )),
            }
        }
        errors
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn ensure_native_platform_supported(_window: &Window) -> Result<(), String> {
    Ok(())
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn ensure_native_platform_supported(_window: &Window) -> Result<(), String> {
    Err("当前平台不支持全局快捷键".to_owned())
}

/// 把测试或无平台映射场景中的 GPUI 按键转换为全局快捷键。
/// 仅按下修饰键时返回 `None`，调用方继续等待主键。
#[cfg(test)]
pub(crate) fn shortcut_from_keystroke(
    keystroke: &Keystroke,
) -> Result<Option<KeyboardShortcut>, String> {
    shortcut_from_parts(&keystroke.key, &keystroke.modifiers)
}

/// 使用当前键盘布局补齐 Windows 的基础键与隐含 Shift，再转换为全局快捷键。
pub(crate) fn shortcut_from_keybinding(
    keystroke: &KeybindingKeystroke,
) -> Result<Option<KeyboardShortcut>, String> {
    shortcut_from_parts(&keystroke.inner().key, keystroke.modifiers())
}

fn shortcut_from_parts(
    source_key: &str,
    source_modifiers: &gpui::Modifiers,
) -> Result<Option<KeyboardShortcut>, String> {
    let key = source_key.to_ascii_lowercase();
    if is_modifier_key(&key) {
        return Ok(None);
    }
    if source_modifiers.function {
        return Err("Fn 不能作为全局快捷键修饰键".to_owned());
    }
    let code = code_from_gpui_key(&key)
        .ok_or_else(|| format!("当前按键不支持全局快捷键：{source_key}"))?;
    let mut modifiers = Modifiers::empty();
    if source_modifiers.control {
        modifiers |= Modifiers::CONTROL;
    }
    if source_modifiers.alt {
        modifiers |= Modifiers::ALT;
    }
    if source_modifiers.shift {
        modifiers |= Modifiers::SHIFT;
    }
    if source_key.len() == 1
        && (source_key.as_bytes()[0].is_ascii_uppercase()
            || matches!(
                key.as_str(),
                "~" | "|"
                    | "{"
                    | "}"
                    | "<"
                    | ")"
                    | "!"
                    | "@"
                    | "#"
                    | "$"
                    | "%"
                    | "^"
                    | "&"
                    | "*"
                    | "("
                    | "+"
                    | "_"
                    | ">"
                    | "\""
                    | ":"
                    | "?"
            ))
    {
        modifiers |= Modifiers::SHIFT;
    }
    if source_modifiers.platform {
        modifiers |= Modifiers::SUPER;
    }
    KeyboardShortcut::from_parts(modifiers, code).map(Some)
}

/// 返回适合逐个渲染为键帽的短标签。
pub(crate) fn shortcut_keycaps(shortcut: KeyboardShortcut) -> Vec<String> {
    let modifiers = shortcut.modifiers();
    let mut labels = Vec::with_capacity(5);
    if modifiers.contains(Modifiers::CONTROL) {
        labels.push("Ctrl".to_owned());
    }
    if modifiers.contains(Modifiers::ALT) {
        labels.push(if cfg!(target_os = "macos") {
            "Option".to_owned()
        } else {
            "Alt".to_owned()
        });
    }
    if modifiers.contains(Modifiers::SHIFT) {
        labels.push("Shift".to_owned());
    }
    if modifiers.contains(Modifiers::SUPER) {
        labels.push(if cfg!(target_os = "macos") {
            "Cmd".to_owned()
        } else if cfg!(target_os = "windows") {
            "Win".to_owned()
        } else {
            "Super".to_owned()
        });
    }
    labels.push(keycap_label(shortcut.key()));
    labels
}

fn is_modifier_key(key: &str) -> bool {
    matches!(
        key,
        "alt"
            | "option"
            | "control"
            | "ctrl"
            | "command"
            | "cmd"
            | "fn"
            | "meta"
            | "shift"
            | "super"
            | "win"
    )
}

fn code_from_gpui_key(key: &str) -> Option<Code> {
    Some(match key {
        "a" => Code::KeyA,
        "b" => Code::KeyB,
        "c" => Code::KeyC,
        "d" => Code::KeyD,
        "e" => Code::KeyE,
        "f" => Code::KeyF,
        "g" => Code::KeyG,
        "h" => Code::KeyH,
        "i" => Code::KeyI,
        "j" => Code::KeyJ,
        "k" => Code::KeyK,
        "l" => Code::KeyL,
        "m" => Code::KeyM,
        "n" => Code::KeyN,
        "o" => Code::KeyO,
        "p" => Code::KeyP,
        "q" => Code::KeyQ,
        "r" => Code::KeyR,
        "s" => Code::KeyS,
        "t" => Code::KeyT,
        "u" => Code::KeyU,
        "v" => Code::KeyV,
        "w" => Code::KeyW,
        "x" => Code::KeyX,
        "y" => Code::KeyY,
        "z" => Code::KeyZ,
        "0" | ")" => Code::Digit0,
        "1" | "!" => Code::Digit1,
        "2" | "@" => Code::Digit2,
        "3" | "#" => Code::Digit3,
        "4" | "$" => Code::Digit4,
        "5" | "%" => Code::Digit5,
        "6" | "^" => Code::Digit6,
        "7" | "&" => Code::Digit7,
        "8" | "*" => Code::Digit8,
        "9" | "(" => Code::Digit9,
        "`" | "~" | "backquote" => Code::Backquote,
        "\\" | "|" | "backslash" => Code::Backslash,
        "[" | "{" | "bracketleft" => Code::BracketLeft,
        "]" | "}" | "bracketright" => Code::BracketRight,
        "," | "<" | "comma" => Code::Comma,
        "=" | "+" | "equal" => Code::Equal,
        "-" | "_" | "minus" => Code::Minus,
        "." | ">" | "period" => Code::Period,
        "'" | "\"" | "quote" => Code::Quote,
        ";" | ":" | "semicolon" => Code::Semicolon,
        "/" | "?" | "slash" => Code::Slash,
        "backspace" => Code::Backspace,
        "capslock" => Code::CapsLock,
        "enter" | "return" => Code::Enter,
        "space" | " " => Code::Space,
        "tab" => Code::Tab,
        "delete" | "forwarddelete" => Code::Delete,
        "end" => Code::End,
        "home" => Code::Home,
        "insert" => Code::Insert,
        "pagedown" => Code::PageDown,
        "pageup" => Code::PageUp,
        "down" | "arrowdown" => Code::ArrowDown,
        "left" | "arrowleft" => Code::ArrowLeft,
        "right" | "arrowright" => Code::ArrowRight,
        "up" | "arrowup" => Code::ArrowUp,
        "escape" | "esc" => Code::Escape,
        "printscreen" => Code::PrintScreen,
        "scrolllock" => Code::ScrollLock,
        "pause" => Code::Pause,
        "numpad0" => Code::Numpad0,
        "numpad1" => Code::Numpad1,
        "numpad2" => Code::Numpad2,
        "numpad3" => Code::Numpad3,
        "numpad4" => Code::Numpad4,
        "numpad5" => Code::Numpad5,
        "numpad6" => Code::Numpad6,
        "numpad7" => Code::Numpad7,
        "numpad8" => Code::Numpad8,
        "numpad9" => Code::Numpad9,
        "numpadadd" => Code::NumpadAdd,
        "numpaddecimal" => Code::NumpadDecimal,
        "numpaddivide" => Code::NumpadDivide,
        "numpadmultiply" => Code::NumpadMultiply,
        "numpadsubtract" => Code::NumpadSubtract,
        "f1" => Code::F1,
        "f2" => Code::F2,
        "f3" => Code::F3,
        "f4" => Code::F4,
        "f5" => Code::F5,
        "f6" => Code::F6,
        "f7" => Code::F7,
        "f8" => Code::F8,
        "f9" => Code::F9,
        "f10" => Code::F10,
        "f11" => Code::F11,
        "f12" => Code::F12,
        "f13" => Code::F13,
        "f14" => Code::F14,
        "f15" => Code::F15,
        "f16" => Code::F16,
        "f17" => Code::F17,
        "f18" => Code::F18,
        "f19" => Code::F19,
        "f20" => Code::F20,
        "f21" => Code::F21,
        "f22" => Code::F22,
        "f23" => Code::F23,
        "f24" => Code::F24,
        _ => return None,
    })
}

fn keycap_label(code: Code) -> String {
    match code {
        Code::ArrowDown => "Down".to_owned(),
        Code::ArrowLeft => "Left".to_owned(),
        Code::ArrowRight => "Right".to_owned(),
        Code::ArrowUp => "Up".to_owned(),
        Code::Backquote => "`".to_owned(),
        Code::Backslash => "\\".to_owned(),
        Code::BracketLeft => "[".to_owned(),
        Code::BracketRight => "]".to_owned(),
        Code::Comma => ",".to_owned(),
        Code::Equal => "=".to_owned(),
        Code::Minus => "-".to_owned(),
        Code::Period => ".".to_owned(),
        Code::Quote => "'".to_owned(),
        Code::Semicolon => ";".to_owned(),
        Code::Slash => "/".to_owned(),
        Code::Space => "Space".to_owned(),
        _ => {
            let label = code.to_string();
            label
                .strip_prefix("Key")
                .or_else(|| label.strip_prefix("Digit"))
                .unwrap_or(&label)
                .to_owned()
        }
    }
}
