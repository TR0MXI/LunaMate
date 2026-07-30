//! 将宿主截图授权收窄为单个 Provider 请求可用的能力。

use std::{future::Future, pin::Pin, sync::Arc};

use lunamate_agent::{
    ScreenshotCapability,
    media::{ImageAttachment, ImageInputError},
};

use crate::config::CONFIG;

/// 把一次已持久化的宿主截图授权收窄为单个 Provider 请求可用的能力。
struct HostScreenshotCapability {
    permission_revision: u64,
}

impl ScreenshotCapability for HostScreenshotCapability {
    fn is_authorized(&self) -> bool {
        CONFIG.agent_screenshot_permission_is_current(self.permission_revision)
    }

    fn wait_for_revocation(&self) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        let permission_revision = self.permission_revision;
        let mut revisions = CONFIG.subscribe_agent_screenshot_permission_revision();
        Box::pin(async move {
            while CONFIG.agent_screenshot_permission_is_current(permission_revision) {
                if revisions.changed().await.is_err() {
                    break;
                }
            }
        })
    }

    fn capture(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<ImageAttachment, ImageInputError>> + Send + 'static>>
    {
        let permission_revision = self.permission_revision;
        Box::pin(async move {
            let Some(authorization) = CONFIG.begin_agent_screenshot_capture(permission_revision)
            else {
                return Err(ImageInputError::ScreenCapture);
            };
            // 平台任务必须在启动租约内派发；撤权会等待该临界区结束。
            let capture = tokio::spawn(crate::platform::capture_primary_screen());
            drop(authorization);
            capture.await.map_err(|_| ImageInputError::ScreenCapture)?
        })
    }
}

pub(super) fn host_screenshot_capability() -> Option<Arc<dyn ScreenshotCapability>> {
    CONFIG
        .agent_screenshot_permission_revision()
        .map(|permission_revision| {
            Arc::new(HostScreenshotCapability {
                permission_revision,
            }) as Arc<dyn ScreenshotCapability>
        })
}
