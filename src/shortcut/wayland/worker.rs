//! 驱动 portal 连接，并在连接失败后等待重试或配置变化。

use async_channel::Sender;
use tokio::sync::watch;

use super::{
    PORTAL_RETRY_DELAY, PortalConnectionExit, PortalState, ShortcutEvent, send_runtime_bindings,
    send_runtime_errors, session::run_portal_connection,
};

pub(super) async fn run_worker(
    mut state_receiver: watch::Receiver<PortalState>,
    events: Sender<ShortcutEvent>,
) {
    loop {
        if state_receiver.borrow().shutdown {
            return;
        }
        match run_portal_connection(&mut state_receiver, &events).await {
            Ok(PortalConnectionExit::Shutdown) => return,
            Ok(PortalConnectionExit::Reconfigure) => continue,
            Err(error) => {
                let revision = state_receiver.borrow().revision;
                if !send_runtime_bindings(&events, revision, Vec::new()).await {
                    return;
                }
                if !send_runtime_errors(&events, revision, vec![error]).await {
                    return;
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(PORTAL_RETRY_DELAY) => {}
            changed = state_receiver.changed() => {
                if changed.is_err() || state_receiver.borrow().shutdown {
                    return;
                }
            }
        }
    }
}
