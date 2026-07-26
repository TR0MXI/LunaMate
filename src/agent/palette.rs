//! 从 GPUI Component 主题提取 Agent 视图需要的最小语义色快照。

use gpui::{App, Hsla};
use gpui_component::Theme;

#[derive(Clone, Copy)]
pub(super) struct AgentPalette {
    pub(super) background: Hsla,
    pub(super) foreground: Hsla,
    pub(super) muted_foreground: Hsla,
    pub(super) border: Hsla,
    pub(super) muted: Hsla,
    pub(super) primary: Hsla,
    pub(super) primary_foreground: Hsla,
    pub(super) secondary: Hsla,
    pub(super) accent: Hsla,
    pub(super) sidebar: Hsla,
    pub(super) popover: Hsla,
    pub(super) danger: Hsla,
    pub(super) danger_foreground: Hsla,
}

impl AgentPalette {
    pub(super) fn from_app(cx: &App) -> Self {
        let theme = Theme::global(cx);
        Self {
            background: theme.background,
            foreground: theme.foreground,
            muted_foreground: theme.muted_foreground,
            border: theme.border,
            muted: theme.muted,
            primary: theme.primary,
            primary_foreground: theme.primary_foreground,
            secondary: theme.secondary,
            accent: theme.accent,
            sidebar: theme.sidebar,
            popover: theme.popover,
            danger: theme.danger,
            danger_foreground: theme.danger_foreground,
        }
    }
}
