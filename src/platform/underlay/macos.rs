//! 在 GPUI 的 AppKit view 下方插入承载 CAMetalLayer 的 sibling view。

use std::{ffi::c_void, ptr::NonNull, sync::OnceLock};

use cocoa::{
    appkit::{NSView, NSViewHeightSizable, NSViewWidthSizable, NSWindowOrderingMode},
    base::{BOOL, YES, id, nil},
};
use gpui::Window;
use gpui_wgpu::wgpu;
use objc::{
    declare::ClassDecl,
    msg_send,
    runtime::{Class, Object, Sel},
    sel, sel_impl,
};
use raw_window_handle::{HasWindowHandle, RawDisplayHandle, RawWindowHandle};

use super::{InitializationCancellation, SurfaceSeed, UnderlaySize};

/// 由 worker 持有并在 present 前维护 CAMetalLayer 的后台属性。
pub(crate) struct SurfaceOwner {
    render_layer: usize,
}

impl SurfaceOwner {
    /// 恢复有限的 nextDrawable 等待，确保隐藏窗口仍能响应关闭。
    pub(crate) fn prepare_present(&mut self, _size: UnderlaySize) -> Result<(), String> {
        let render_layer = self.render_layer as *mut Object;
        // SAFETY: render_layer 是 WGPU surface 持有的 CAMetalLayer，Core Animation layer
        // 属性允许跨线程设置。恢复超时可确保隐藏窗口和 shutdown 不会永久卡在 nextDrawable。
        unsafe {
            let responds: BOOL = msg_send![
                render_layer,
                respondsToSelector: sel!(setAllowsNextDrawableTimeout:)
            ];
            if responds == YES {
                let _: () = msg_send![render_layer, setAllowsNextDrawableTimeout: YES];
            }
        }
        Ok(())
    }
}

/// UI 线程持有的 sibling NSView 与 layer delegate。
pub(crate) struct NativeAttachment {
    view: NonNull<Object>,
    render_layer: Option<NonNull<Object>>,
    layer_delegate: NonNull<Object>,
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
    let gpui_view = handle.ns_view.as_ptr().cast::<Object>();
    // SAFETY: raw-window-handle 保证指针在 WindowHandle 生命周期内指向当前 GPUI NSView。
    // 所有 AppKit 消息均在 GPUI 主线程执行，新 view 由本结构持有一个 +1 引用。
    let underlay = unsafe {
        let superview: id = msg_send![gpui_view, superview];
        if superview == nil {
            return Err("GPUI macOS view 没有可用 superview".to_owned());
        }
        let frame = NSView::frame(gpui_view.cast());
        let allocated: id = msg_send![objc::class!(NSView), alloc];
        let underlay = NSView::initWithFrame_(allocated, frame);
        if underlay == nil {
            return Err("无法创建 Live2D macOS underlay view".to_owned());
        }
        underlay.setAutoresizingMask_(NSViewWidthSizable | NSViewHeightSizable);
        let _: () = msg_send![
            superview,
            addSubview: underlay
            positioned: NSWindowOrderingMode::NSWindowBelow
            relativeTo: gpui_view
        ];
        underlay
    };
    let underlay_ptr =
        NonNull::new(underlay.cast::<Object>()).expect("前面的 NSView 初始化分支已经拒绝 nil 指针");
    let layer_delegate = match create_layer_delegate() {
        Ok(delegate) => delegate,
        Err(error) => {
            // SAFETY: underlay 是当前主线程刚以 alloc/init 创建的 +1 NSView；尚无 surface
            // 或其他持有者，先移出层级再释放即可完整回滚 attachment。
            unsafe {
                let _: () = msg_send![underlay, removeFromSuperview];
                let _: () = msg_send![underlay, release];
            }
            return Err(error);
        }
    };
    let mut attachment = NativeAttachment {
        view: underlay_ptr,
        render_layer: None,
        layer_delegate,
    };

    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::METAL;
    let instance = wgpu::Instance::new(descriptor);
    let raw_window_handle = RawWindowHandle::AppKit(raw_window_handle::AppKitWindowHandle::new(
        underlay_ptr.cast::<c_void>(),
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
        let root_layer: id = msg_send![underlay, layer];
        let sublayers: id = msg_send![root_layer, sublayers];
        let render_layer: id = msg_send![sublayers, lastObject];
        let render_layer = NonNull::new(render_layer.cast::<Object>())
            .ok_or_else(|| "WGPU 未在 macOS underlay view 中建立 CAMetalLayer".to_owned())?;
        let _: () = msg_send![render_layer.as_ptr(), setDelegate: layer_delegate.as_ptr()];
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
            let view = self.view.as_ptr();
            if let Some(render_layer) = self.render_layer {
                let _: () = msg_send![render_layer.as_ptr(), setDelegate: nil];
            }
            let _: () = msg_send![view, removeFromSuperview];
            let _: () = msg_send![view, release];
            let _: () = msg_send![self.layer_delegate.as_ptr(), release];
        }
    }
}

extern "C" fn layer_delegate_window(_: &Object, _: Sel) -> id {
    nil
}

fn create_layer_delegate() -> Result<NonNull<Object>, String> {
    static CLASS: OnceLock<&'static Class> = OnceLock::new();
    let class = CLASS.get_or_init(|| {
        let superclass =
            Class::get("NSObject").expect("macOS 进程必须提供 NSObject 作为 Objective-C 根类");
        let mut declaration = ClassDecl::new("LunaMateMetalLayerDelegate", superclass)
            .expect("Live2D CAMetalLayer delegate 类只应注册一次");
        // SAFETY: `window` selector 无参数并返回 Objective-C 对象；实现使用完全匹配的 ABI。
        unsafe {
            declaration.add_method(
                sel!(window),
                layer_delegate_window as extern "C" fn(&Object, Sel) -> id,
            );
        }
        declaration.register()
    });
    // SAFETY: Objective-C `new` 返回当前线程拥有的 +1 NSObject 引用。
    let delegate: id = unsafe { msg_send![*class, new] };
    NonNull::new(delegate.cast::<Object>())
        .ok_or_else(|| "无法创建 Live2D CAMetalLayer delegate".to_owned())
}
