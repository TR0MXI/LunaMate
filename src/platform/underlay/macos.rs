//! 在 GPUI 的 AppKit view 下方插入承载 CAMetalLayer 的 sibling view。

use std::{ffi::c_void, ptr::NonNull, sync::OnceLock};

use gpui::Window;
use gpui_wgpu::wgpu;
use objc2::{
    MainThreadMarker, msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel},
    sel,
};
use objc2_app_kit::{NSAutoresizingMaskOptions, NSView, NSWindowOrderingMode};
use raw_window_handle::{HasWindowHandle, RawDisplayHandle, RawWindowHandle};

use super::{InitializationCancellation, SurfaceSeed, UnderlaySize};

/// 由 worker 持有并在 present 前维护 CAMetalLayer 的后台属性。
pub(crate) struct SurfaceOwner {
    render_layer: usize,
}

impl SurfaceOwner {
    /// 恢复有限的 nextDrawable 等待，确保隐藏窗口仍能响应关闭。
    pub(crate) fn prepare_present(&mut self, _size: UnderlaySize) -> Result<(), String> {
        let render_layer = self.render_layer as *mut AnyObject;
        // SAFETY: render_layer 是 WGPU surface 持有的 CAMetalLayer，Core Animation layer
        // 属性允许跨线程设置。恢复超时可确保隐藏窗口和 shutdown 不会永久卡在 nextDrawable。
        unsafe {
            let responds: Bool = msg_send![
                render_layer,
                respondsToSelector: sel!(setAllowsNextDrawableTimeout:)
            ];
            if responds.as_bool() {
                let _: () = msg_send![render_layer, setAllowsNextDrawableTimeout: true];
            }
        }
        Ok(())
    }
}

/// UI 线程持有的 sibling NSView 与 layer delegate。
pub(crate) struct NativeAttachment {
    view: Retained<NSView>,
    render_layer: Option<NonNull<AnyObject>>,
    layer_delegate: Retained<AnyObject>,
}

/// 把主线程创建的 Metal surface 转交给 GPU worker。
pub(crate) struct SurfaceFactory {
    seed: SurfaceSeed,
}

/// 在 GPUI view 下方插入透明 sibling view 并创建 Metal surface。
pub(super) fn attach(
    window: &Window,
) -> Result<Option<(SurfaceFactory, NativeAttachment)>, String> {
    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| format!("无法取得 GPUI macOS 窗口句柄：{error}"))?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Ok(None);
    };
    // SAFETY: raw-window-handle 保证该指针在 WindowHandle 生命周期内指向当前 GPUI NSView。
    let gpui_view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    let main_thread = MainThreadMarker::new()
        .ok_or_else(|| "macOS underlay 只能在 AppKit 主线程创建".to_owned())?;
    // SAFETY: `superview` 是未保留属性；GPUI NSView 与其层级在当前主线程调用期间保持存活。
    let underlay = unsafe {
        let superview = gpui_view
            .superview()
            .ok_or_else(|| "GPUI macOS view 没有可用 superview".to_owned())?;
        let underlay = NSView::initWithFrame(main_thread.alloc(), gpui_view.frame());
        let autoresizing_mask = NSAutoresizingMaskOptions::ViewWidthSizable
            | NSAutoresizingMaskOptions::ViewHeightSizable;
        underlay.setAutoresizingMask(autoresizing_mask);
        superview.addSubview_positioned_relativeTo(
            &underlay,
            NSWindowOrderingMode::Below,
            Some(gpui_view),
        );
        underlay
    };
    let layer_delegate = match create_layer_delegate() {
        Ok(delegate) => delegate,
        Err(error) => {
            underlay.removeFromSuperview();
            return Err(error);
        }
    };
    let mut attachment = NativeAttachment {
        view: underlay,
        render_layer: None,
        layer_delegate,
    };

    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::METAL;
    let instance = wgpu::Instance::new(descriptor);
    let raw_window_handle = RawWindowHandle::AppKit(raw_window_handle::AppKitWindowHandle::new(
        NonNull::from(&*attachment.view).cast::<c_void>(),
    ));
    let raw_display_handle =
        RawDisplayHandle::AppKit(raw_window_handle::AppKitDisplayHandle::new());
    // SAFETY: underlay 是当前主线程创建并由 NativeAttachment 保持存活的 NSView；surface
    // 在 attachment 从 superview 移除和释放前由 worker 完整销毁。
    let surface = unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(raw_display_handle),
            raw_window_handle,
        })
    }
    .map_err(|error| format!("无法创建 macOS Live2D Metal surface：{error}"))?;
    // SAFETY: raw-window-metal 已把 CAMetalLayer 追加到仍存活的 underlay backing layer；
    // lastObject 因此是该 layer。delegate 由 attachment 保持到 surface 完整销毁之后。
    let render_layer = unsafe {
        let root_layer: *mut AnyObject = msg_send![&*attachment.view, layer];
        let sublayers: *mut AnyObject = msg_send![root_layer, sublayers];
        let render_layer: *mut AnyObject = msg_send![sublayers, lastObject];
        let render_layer = NonNull::new(render_layer)
            .ok_or_else(|| "WGPU 未在 macOS underlay view 中建立 CAMetalLayer".to_owned())?;
        let _: () = msg_send![
            render_layer.as_ptr(),
            setDelegate: &*attachment.layer_delegate
        ];
        render_layer
    };
    attachment.render_layer = Some(render_layer);

    Ok(Some((
        SurfaceFactory {
            seed: SurfaceSeed::new(
                instance,
                surface,
                SurfaceOwner {
                    render_layer: render_layer.as_ptr() as usize,
                },
            ),
        },
        attachment,
    )))
}

impl SurfaceFactory {
    /// 将已经建立的 surface 种子移动到 GPU worker。
    pub(crate) fn create(
        self,
        _cancellation: &dyn InitializationCancellation,
    ) -> Result<SurfaceSeed, String> {
        Ok(self.seed)
    }
}

impl Drop for NativeAttachment {
    fn drop(&mut self) {
        // SAFETY: GpuUnderlay 只在 GPUI 主线程析构 attachment，并且持有由 alloc/init
        // 获得的 +1 引用。worker 已在此之前 join，CAMetalLayer 不再被 surface 使用。
        unsafe {
            if let Some(render_layer) = self.render_layer {
                let _: () = msg_send![
                    render_layer.as_ptr(),
                    setDelegate: std::ptr::null_mut::<AnyObject>()
                ];
            }
        }
        self.view.removeFromSuperview();
    }
}

extern "C" fn layer_delegate_window(_: *mut AnyObject, _: Sel) -> *mut AnyObject {
    std::ptr::null_mut()
}

fn create_layer_delegate() -> Result<Retained<AnyObject>, String> {
    static CLASS: OnceLock<&'static AnyClass> = OnceLock::new();
    let class = CLASS.get_or_init(|| {
        let superclass =
            AnyClass::get(c"NSObject").expect("macOS 进程必须提供 NSObject 作为 Objective-C 根类");
        let mut declaration = ClassBuilder::new(c"LunaMateMetalLayerDelegate", superclass)
            .expect("Live2D CAMetalLayer delegate 类只应注册一次");
        // SAFETY: `window` selector 无参数并返回 Objective-C 对象；实现使用完全匹配的 ABI。
        let func: extern "C" fn(*mut AnyObject, Sel) -> *mut AnyObject = layer_delegate_window;
        unsafe {
            declaration.add_method::<AnyObject, _>(sel!(window), func);
        }
        declaration.register()
    });
    // SAFETY: Objective-C `new` 返回当前线程拥有的 +1 NSObject 引用。
    let delegate = unsafe { msg_send![*class, new] };
    Ok(delegate)
}
