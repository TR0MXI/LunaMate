//! 处理 worker 的暂停、surface 重试、模型替换与关闭等待。

use std::time::{Duration, Instant};

use super::super::{LoadRequest, WorkerMailbox};
use super::{PauseWaitResult, RetryWaitResult};

pub(in crate::model) fn wait_while_paused(mailbox: &WorkerMailbox) -> PauseWaitResult {
    let mut woken = false;
    while mailbox.is_paused() {
        let update = mailbox.wait(None);
        woken |= update.woken;
        if update.shutdown {
            return PauseWaitResult::Shutdown;
        }
        if let Some(replacement) = update.replacement {
            if woken {
                mailbox.wake();
            }
            return PauseWaitResult::Replaced(replacement);
        }
    }
    if woken {
        mailbox.wake();
    }
    PauseWaitResult::Running
}

pub(in crate::model) fn wait_for_surface_retry(
    mailbox: &WorkerMailbox,
    delay: Duration,
) -> RetryWaitResult {
    let deadline = Instant::now() + delay;
    let mut woken = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let update = mailbox.wait(Some(remaining));
        woken |= update.woken;
        if update.shutdown {
            return RetryWaitResult::Shutdown;
        }
        if let Some(replacement) = update.replacement {
            if woken {
                mailbox.wake();
            }
            return RetryWaitResult::Replaced(replacement);
        }
        if update.paused {
            if woken {
                mailbox.wake();
            }
            return RetryWaitResult::Paused;
        }
        if Instant::now() >= deadline {
            if woken {
                mailbox.wake();
            }
            return RetryWaitResult::Ready;
        }
    }
}

pub(super) fn wait_for_replacement(mailbox: &WorkerMailbox) -> Option<LoadRequest> {
    let mut woken = false;
    loop {
        let update = mailbox.wait(None);
        woken |= update.woken;
        if update.shutdown {
            return None;
        }
        if let Some(replacement) = update.replacement {
            if woken {
                mailbox.wake();
            }
            return Some(replacement);
        }
    }
}
