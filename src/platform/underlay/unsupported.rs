//! 为不支持原生 GPU underlay 的平台提供无 attachment 降级实现。

use gpui::Window;

use super::{InitializationCancellation, SurfaceSeed, UnderlaySize};

pub(crate) struct SurfaceFactory;
pub(crate) struct NativeAttachment;
pub(crate) struct SurfaceOwner;

pub(super) fn attach(
    _window: &Window,
) -> Result<Option<(SurfaceFactory, NativeAttachment)>, String> {
    Ok(None)
}

impl SurfaceOwner {
    pub(crate) fn prepare_present(&mut self, _size: UnderlaySize) -> Result<(), String> {
        Ok(())
    }
}

impl SurfaceFactory {
    pub(crate) fn create(
        self,
        _cancellation: &dyn InitializationCancellation,
    ) -> Result<SurfaceSeed, String> {
        Err("当前平台不支持 Live2D GPU underlay".to_owned())
    }
}
