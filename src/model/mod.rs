//! 统一暴露模型目录、Live2D 运行时、交互帧和 GPU underlay 能力。
//!
//! 应用层只依赖本模块的稳定接口；资源解析、动画与渲染实现保持为私有子模块，原生
//! underlay attachment 由 `crate::platform` 提供。

mod animation;
mod capabilities;
mod catalog;
mod expression;
mod frame_scheduler;
mod gpu_underlay;
mod interaction;
mod live2d;

#[cfg(test)]
mod tests;

pub(crate) use capabilities::ModelLoadDiagnostics;
#[cfg(test)]
pub(crate) use capabilities::{ModelDiagnosticCategory, ModelLoadDiagnostic};
pub(crate) use catalog::{ModelCatalog, ModelFamily};
pub(crate) use frame_scheduler::{
    FramePacer, FrameRateMeter, FrameWake, FrameWakeReceiver, frame_wake_channel,
};
pub(crate) use gpu_underlay::{GpuUnderlay, GpuUnderlayEvent, GpuUnderlaySize};
pub(crate) use interaction::{
    MAX_COMMANDS_PER_FRAME, ModelCommand, ModelCommandSender, RenderedModelFrame, command_channel,
};
pub(crate) use live2d::{
    AnimatedModel, ModelLoadError, ModelPreviewCapabilities, RenderCancellation, RenderError,
};
