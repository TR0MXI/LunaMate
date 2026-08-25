//! 定义全局快捷键动作、稳定表示和配置文件读写。

use std::collections::HashSet;

#[cfg(not(target_os = "linux"))]
use global_hotkey::hotkey::HotKey;
use keyboard_types::{Code, Modifiers};
use toml_edit::{DocumentMut, Value};

use super::{ConfigWriteError, ensure_table_like, remove_key, set_item_value};

/// 可由全局快捷键触发的应用动作。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ShortcutAction {
    VoiceInput,
    ToggleDesktopPet,
    ToggleSettings,
    ToggleChatInput,
}

impl ShortcutAction {
    pub const ALL: [Self; 4] = [
        Self::VoiceInput,
        Self::ToggleDesktopPet,
        Self::ToggleSettings,
        Self::ToggleChatInput,
    ];

    /// 返回日志和配置文件使用的稳定动作标识。
    pub const fn id(self) -> &'static str {
        match self {
            Self::VoiceInput => "voice_input",
            Self::ToggleDesktopPet => "toggle_desktop_pet",
            Self::ToggleSettings => "toggle_settings",
            Self::ToggleChatInput => "toggle_chat_input",
        }
    }

    #[cfg(target_os = "linux")]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.id() == id)
    }
}

/// 一组已经规范化、可交给系统注册或 XDG portal 使用的键盘按键。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct KeyboardShortcut {
    modifiers: Modifiers,
    key: Code,
}

impl KeyboardShortcut {
    /// 从配置文件中的稳定标识恢复快捷键；Esc 专用于录入时清空。
    pub fn from_id(id: &str) -> Result<Self, String> {
        let mut parts = id.split('+').map(str::trim).collect::<Vec<_>>();
        let key = parts.pop().ok_or_else(|| "快捷键缺少主键".to_owned())?;
        if key.is_empty() {
            return Err("快捷键缺少主键".to_owned());
        }
        let mut modifiers = Modifiers::empty();
        for modifier in parts {
            if modifier.is_empty() {
                return Err("快捷键包含空按键段".to_owned());
            }
            match modifier.to_ascii_uppercase().as_str() {
                "SHIFT" => modifiers |= Modifiers::SHIFT,
                "CONTROL" | "CTRL" => modifiers |= Modifiers::CONTROL,
                "ALT" | "OPTION" => modifiers |= Modifiers::ALT,
                "SUPER" | "COMMAND" | "CMD" => modifiers |= Modifiers::SUPER,
                "COMMANDORCONTROL" | "COMMANDORCTRL" | "CMDORCTRL" | "CMDORCONTROL" => {
                    if cfg!(target_os = "macos") {
                        modifiers |= Modifiers::SUPER;
                    } else {
                        modifiers |= Modifiers::CONTROL;
                    }
                }
                _ => return Err(format!("无法识别快捷键修饰键：{modifier}")),
            }
        }
        let key = normalize_key(key)?;
        Self::from_parts(modifiers, key)
    }

    pub fn from_parts(mut modifiers: Modifiers, key: Code) -> Result<Self, String> {
        if key == Code::Escape {
            return Err("Esc 保留用于清空快捷键".to_owned());
        }
        if !key_is_supported(key) {
            return Err(format!("当前按键不受全局快捷键支持：{key}"));
        }
        if modifiers.contains(Modifiers::META) {
            modifiers.remove(Modifiers::META);
            modifiers.insert(Modifiers::SUPER);
        }
        let supported = Modifiers::SHIFT | Modifiers::CONTROL | Modifiers::ALT | Modifiers::SUPER;
        if modifiers.intersects(!supported) {
            return Err("快捷键包含不支持的修饰键".to_owned());
        }
        Ok(Self { modifiers, key })
    }

    /// 返回不依赖 TOML 布局的规范化快捷键标识。
    pub fn id(self) -> String {
        let mut parts = Vec::with_capacity(5);
        if self.modifiers.contains(Modifiers::SHIFT) {
            parts.push("shift".to_owned());
        }
        if self.modifiers.contains(Modifiers::CONTROL) {
            parts.push("control".to_owned());
        }
        if self.modifiers.contains(Modifiers::ALT) {
            parts.push("alt".to_owned());
        }
        if self.modifiers.contains(Modifiers::SUPER) {
            parts.push("super".to_owned());
        }
        parts.push(self.key.to_string());
        parts.join("+")
    }

    pub const fn modifiers(self) -> Modifiers {
        self.modifiers
    }

    pub const fn key(self) -> Code {
        self.key
    }

    #[cfg(not(target_os = "linux"))]
    pub fn hotkey(self) -> HotKey {
        HotKey::new(Some(self.modifiers), self.key)
    }
}

fn key_is_supported(key: Code) -> bool {
    matches!(
        key,
        Code::Backquote
            | Code::Backslash
            | Code::BracketLeft
            | Code::BracketRight
            | Code::Comma
            | Code::Digit0
            | Code::Digit1
            | Code::Digit2
            | Code::Digit3
            | Code::Digit4
            | Code::Digit5
            | Code::Digit6
            | Code::Digit7
            | Code::Digit8
            | Code::Digit9
            | Code::Equal
            | Code::KeyA
            | Code::KeyB
            | Code::KeyC
            | Code::KeyD
            | Code::KeyE
            | Code::KeyF
            | Code::KeyG
            | Code::KeyH
            | Code::KeyI
            | Code::KeyJ
            | Code::KeyK
            | Code::KeyL
            | Code::KeyM
            | Code::KeyN
            | Code::KeyO
            | Code::KeyP
            | Code::KeyQ
            | Code::KeyR
            | Code::KeyS
            | Code::KeyT
            | Code::KeyU
            | Code::KeyV
            | Code::KeyW
            | Code::KeyX
            | Code::KeyY
            | Code::KeyZ
            | Code::Minus
            | Code::Period
            | Code::Quote
            | Code::Semicolon
            | Code::Slash
            | Code::Backspace
            | Code::CapsLock
            | Code::Enter
            | Code::Space
            | Code::Tab
            | Code::Delete
            | Code::End
            | Code::Home
            | Code::Insert
            | Code::PageDown
            | Code::PageUp
            | Code::ArrowDown
            | Code::ArrowLeft
            | Code::ArrowRight
            | Code::ArrowUp
            | Code::Numpad0
            | Code::Numpad1
            | Code::Numpad2
            | Code::Numpad3
            | Code::Numpad4
            | Code::Numpad5
            | Code::Numpad6
            | Code::Numpad7
            | Code::Numpad8
            | Code::Numpad9
            | Code::NumpadAdd
            | Code::NumpadDecimal
            | Code::NumpadDivide
            | Code::NumpadMultiply
            | Code::NumpadSubtract
            | Code::PrintScreen
            | Code::ScrollLock
            | Code::Pause
            | Code::F1
            | Code::F2
            | Code::F3
            | Code::F4
            | Code::F5
            | Code::F6
            | Code::F7
            | Code::F8
            | Code::F9
            | Code::F10
            | Code::F11
            | Code::F12
            | Code::F13
            | Code::F14
            | Code::F15
            | Code::F16
            | Code::F17
            | Code::F18
            | Code::F19
            | Code::F20
            | Code::F21
            | Code::F22
            | Code::F23
            | Code::F24
    )
}
fn normalize_key(key: &str) -> Result<Code, String> {
    let uppercase = key.to_ascii_uppercase();
    if let Some(letter) = uppercase.strip_prefix("KEY").filter(|key| key.len() == 1)
        && letter.as_bytes()[0].is_ascii_alphabetic()
    {
        return format!("Key{letter}").parse().map_err(|_| key_error(key));
    }
    if uppercase.len() == 1 && uppercase.as_bytes()[0].is_ascii_alphabetic() {
        return format!("Key{uppercase}")
            .parse()
            .map_err(|_| key_error(key));
    }
    if let Some(digit) = uppercase.strip_prefix("DIGIT").filter(|key| key.len() == 1)
        && digit.as_bytes()[0].is_ascii_digit()
    {
        return format!("Digit{digit}").parse().map_err(|_| key_error(key));
    }
    if uppercase.len() == 1 && uppercase.as_bytes()[0].is_ascii_digit() {
        return format!("Digit{uppercase}")
            .parse()
            .map_err(|_| key_error(key));
    }
    if let Some(number) = uppercase.strip_prefix('F')
        && let Ok(number) = number.parse::<u8>()
        && (1..=24).contains(&number)
    {
        return format!("F{number}").parse().map_err(|_| key_error(key));
    }
    if let Some(number) = uppercase.strip_prefix("NUMPAD")
        && number.len() == 1
        && number.as_bytes()[0].is_ascii_digit()
    {
        return format!("Numpad{number}")
            .parse()
            .map_err(|_| key_error(key));
    }
    let normalized = match uppercase.as_str() {
        "BACKQUOTE" | "`" => "Backquote",
        "BACKSLASH" | "\\" => "Backslash",
        "BRACKETLEFT" | "[" => "BracketLeft",
        "BRACKETRIGHT" | "]" => "BracketRight",
        "COMMA" | "," => "Comma",
        "EQUAL" | "=" => "Equal",
        "MINUS" | "-" => "Minus",
        "PERIOD" | "." => "Period",
        "QUOTE" | "'" => "Quote",
        "SEMICOLON" | ";" => "Semicolon",
        "SLASH" | "/" => "Slash",
        "BACKSPACE" => "Backspace",
        "CAPSLOCK" => "CapsLock",
        "ENTER" | "RETURN" => "Enter",
        "SPACE" | " " => "Space",
        "TAB" => "Tab",
        "DELETE" | "FORWARDDELETE" => "Delete",
        "END" => "End",
        "HOME" => "Home",
        "INSERT" => "Insert",
        "PAGEDOWN" => "PageDown",
        "PAGEUP" => "PageUp",
        "ARROWDOWN" | "DOWN" => "ArrowDown",
        "ARROWLEFT" | "LEFT" => "ArrowLeft",
        "ARROWRIGHT" | "RIGHT" => "ArrowRight",
        "ARROWUP" | "UP" => "ArrowUp",
        "PRINTSCREEN" => "PrintScreen",
        "SCROLLLOCK" => "ScrollLock",
        "PAUSE" | "PAUSEBREAK" => "Pause",
        "NUMPADADD" => "NumpadAdd",
        "NUMPADDECIMAL" => "NumpadDecimal",
        "NUMPADDIVIDE" => "NumpadDivide",
        "NUMPADMULTIPLY" => "NumpadMultiply",
        "NUMPADSUBTRACT" => "NumpadSubtract",
        "ESCAPE" | "ESC" => "Escape",
        _ => key,
    };
    normalized.parse().map_err(|_| key_error(key))
}

fn key_error(key: &str) -> String {
    format!("当前按键不受全局快捷键支持：{key}")
}

/// 四个应用动作的一次性快捷键配置快照；`None` 表示不注册该动作。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ShortcutSettings {
    voice_input: Option<KeyboardShortcut>,
    toggle_desktop_pet: Option<KeyboardShortcut>,
    toggle_settings: Option<KeyboardShortcut>,
    toggle_chat_input: Option<KeyboardShortcut>,
}

impl ShortcutSettings {
    pub fn shortcut(&self, action: ShortcutAction) -> Option<KeyboardShortcut> {
        match action {
            ShortcutAction::VoiceInput => self.voice_input,
            ShortcutAction::ToggleDesktopPet => self.toggle_desktop_pet,
            ShortcutAction::ToggleSettings => self.toggle_settings,
            ShortcutAction::ToggleChatInput => self.toggle_chat_input,
        }
    }

    /// 设置一个动作，并清空占用同一组合的旧动作。
    pub fn assign(
        &mut self,
        action: ShortcutAction,
        shortcut: Option<KeyboardShortcut>,
    ) -> Option<ShortcutAction> {
        let displaced = shortcut.as_ref().and_then(|shortcut| {
            ShortcutAction::ALL.into_iter().find(|candidate| {
                *candidate != action && self.shortcut(*candidate).as_ref() == Some(shortcut)
            })
        });
        if let Some(displaced) = displaced {
            *self.slot_mut(displaced) = None;
        }
        *self.slot_mut(action) = shortcut;
        displaced
    }

    pub fn configured_count(&self) -> usize {
        ShortcutAction::ALL
            .into_iter()
            .filter(|action| self.shortcut(*action).is_some())
            .count()
    }

    pub fn normalized(self) -> Result<Self, ConfigWriteError> {
        let mut used = HashSet::with_capacity(ShortcutAction::ALL.len());
        for action in ShortcutAction::ALL {
            if let Some(shortcut) = self.shortcut(action)
                && !used.insert(shortcut)
            {
                return Err(ConfigWriteError::InvalidValue(format!(
                    "快捷键不能重复绑定：{}",
                    shortcut.id()
                )));
            }
        }
        Ok(self)
    }

    fn slot_mut(&mut self, action: ShortcutAction) -> &mut Option<KeyboardShortcut> {
        match action {
            ShortcutAction::VoiceInput => &mut self.voice_input,
            ShortcutAction::ToggleDesktopPet => &mut self.toggle_desktop_pet,
            ShortcutAction::ToggleSettings => &mut self.toggle_settings,
            ShortcutAction::ToggleChatInput => &mut self.toggle_chat_input,
        }
    }
}

pub fn parse_shortcut_settings(
    document: &DocumentMut,
    warnings: &mut Vec<String>,
) -> ShortcutSettings {
    let mut settings = ShortcutSettings::default();
    let Some(shortcuts) = document.get("shortcuts") else {
        return settings;
    };

    let mut used = HashSet::with_capacity(ShortcutAction::ALL.len());
    for action in ShortcutAction::ALL {
        let Some(item) = shortcuts.get(action.id()) else {
            continue;
        };
        let Some(id) = item.as_str() else {
            warnings.push(format!("shortcuts.{} 必须是字符串，已忽略", action.id()));
            continue;
        };
        if id.trim().is_empty() {
            continue;
        }
        let shortcut = match KeyboardShortcut::from_id(id) {
            Ok(shortcut) => shortcut,
            Err(error) => {
                warnings.push(format!("shortcuts.{} 无效，已忽略：{error}", action.id()));
                continue;
            }
        };
        if !used.insert(shortcut) {
            warnings.push(format!(
                "shortcuts.{} 与已有快捷键重复，已忽略",
                action.id()
            ));
            continue;
        }
        settings.assign(action, Some(shortcut));
    }
    settings
}

pub fn write_shortcut_settings(document: &mut DocumentMut, settings: &ShortcutSettings) {
    ensure_table_like(&mut document["shortcuts"]);
    for action in ShortcutAction::ALL {
        match settings.shortcut(action) {
            Some(shortcut) => set_item_value(
                &mut document["shortcuts"][action.id()],
                Value::from(shortcut.id()),
            ),
            None => remove_key(document, "shortcuts", action.id()),
        }
    }
}
