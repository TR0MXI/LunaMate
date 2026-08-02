//! 按职责组织通用设置的持久化与运行时发布。

mod appearance;
mod logging;
mod voice_tools;
mod window_interaction;

use gpui::Context;

use crate::config::ConfigWriteError;

use super::SettingsView;

pub(in crate::ui) use window_interaction::custom_frame_rate_seed;
#[cfg(test)]
pub(in crate::ui) use window_interaction::parse_custom_frame_rate;

impl SettingsView {
    fn persist_setting(
        &mut self,
        write: impl FnOnce() -> Result<Option<()>, ConfigWriteError> + Send + 'static,
        finish: impl FnOnce(&mut Self, Result<Option<()>, ConfigWriteError>, &mut Context<Self>)
        + 'static,
        cx: &mut Context<Self>,
    ) {
        let background = cx.background_executor().clone();
        let task = cx.spawn(async move |this, cx| {
            let result = background.spawn(async move { write() }).await;
            let _ = this.update(cx, move |this, cx| finish(this, result, cx));
        });
        self.track_write_task(task);
    }
}
