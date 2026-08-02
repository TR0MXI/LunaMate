//! 将应用快捷键序列化为 XDG GlobalShortcuts preferred trigger。

use global_hotkey::hotkey::{Code, Modifiers};

use crate::config::KeyboardShortcut;

pub(in crate::shortcut) fn portal_trigger(shortcut: KeyboardShortcut) -> Result<String, String> {
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
