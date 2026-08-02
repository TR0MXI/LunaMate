//! 通过 XDG GlobalShortcuts portal 管理原生 Wayland 快捷键 session。

mod bindings;
mod session;
mod trigger;
mod worker;

use async_channel::{Receiver, Sender};
use tokio::{runtime::Handle, sync::watch, task::JoinHandle};

use crate::{
    config::ShortcutSettings,
    platform::{WaylandActivationController, WaylandActivationTarget},
};

use super::{ShortcutEvent, ShortcutRuntimeBinding};

#[cfg(test)]
pub(super) use bindings::ActiveBindings;
#[cfg(test)]
pub(super) use trigger::portal_trigger;

const EVENT_CHANNEL_CAPACITY: usize = 32;
const RECONFIGURE_DELAY: std::time::Duration = std::time::Duration::from_millis(150);
const PORTAL_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(5);
const PORTAL_HEALTH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
const PORTAL_HEALTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub(super) struct WaylandShortcutManager {
    state: watch::Sender<PortalState>,
    task: JoinHandle<()>,
    activation: WaylandActivationController,
}

#[derive(Clone)]
struct PortalState {
    revision: u64,
    settings: ShortcutSettings,
    suspended: bool,
    shutdown: bool,
}

enum PortalConnectionExit {
    Shutdown,
    Reconfigure,
}

impl WaylandShortcutManager {
    pub(super) fn new(
        settings: ShortcutSettings,
        target: WaylandActivationTarget,
        runtime: &Handle,
    ) -> (Self, Receiver<ShortcutEvent>) {
        let initial = PortalState {
            revision: 1,
            settings,
            suspended: false,
            shutdown: false,
        };
        let (state, state_receiver) = watch::channel(initial);
        let (event_sender, events) = async_channel::bounded(EVENT_CHANNEL_CAPACITY);
        let task = runtime.spawn(worker::run_worker(state_receiver, event_sender));
        let activation = WaylandActivationController::start(target);
        (
            Self {
                state,
                task,
                activation,
            },
            events,
        )
    }

    pub(super) fn configure(&self, settings: ShortcutSettings) {
        self.state.send_modify(|state| {
            if state.settings != settings {
                state.settings = settings;
                state.revision = state.revision.wrapping_add(1).max(1);
            }
        });
    }

    pub(super) fn set_suspended(&self, suspended: bool) {
        self.state.send_modify(|state| {
            if state.suspended != suspended {
                state.suspended = suspended;
                state.revision = state.revision.wrapping_add(1).max(1);
            }
        });
    }

    pub(super) fn current_revision(&self) -> u64 {
        self.state.borrow().revision
    }

    pub(super) fn activate(&self, token: String) -> Result<(), String> {
        self.activation.activate(token)
    }
}

impl Drop for WaylandShortcutManager {
    fn drop(&mut self) {
        self.state.send_modify(|state| {
            state.shutdown = true;
            state.revision = state.revision.wrapping_add(1).max(1);
        });
        self.task.abort();
    }
}

async fn send_runtime_errors(
    events: &Sender<ShortcutEvent>,
    revision: u64,
    errors: Vec<String>,
) -> bool {
    send_shortcut_event(events, ShortcutEvent::RuntimeErrors { revision, errors }).await
}

async fn send_runtime_bindings(
    events: &Sender<ShortcutEvent>,
    revision: u64,
    bindings: Vec<ShortcutRuntimeBinding>,
) -> bool {
    send_shortcut_event(
        events,
        ShortcutEvent::RuntimeBindings { revision, bindings },
    )
    .await
}

async fn send_shortcut_event(events: &Sender<ShortcutEvent>, event: ShortcutEvent) -> bool {
    events.send(event).await.is_ok()
}
