//! 为不支持原生 GPU underlay 的平台提供无 attachment 降级实现。

use gpui::Window;

pub(super) struct SurfaceFactory;
pub(super) struct NativeAttachment;
pub(super) struct SurfaceOwner;

pub(super) fn attach(
    _window: &Window,
) -> Result<Option<(SurfaceFactory, NativeAttachment)>, String> {
    Ok(None)
}

impl SurfaceOwner {
    pub(super) fn prepare_present(&mut self, _size: super::GpuUnderlaySize) -> Result<(), String> {
        Ok(())
    }
}

impl SurfaceFactory {
    pub(super) fn create(
        self,
        _mailbox: &super::WorkerMailbox,
    ) -> Result<super::SurfaceSeed, String> {
        Err("当前平台不支持 Live2D GPU underlay".to_owned())
    }
}
