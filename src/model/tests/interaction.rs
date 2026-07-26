use crate::model::interaction::{COMMAND_CHANNEL_CAPACITY, ModelCommand, command_channel};

use std::sync::mpsc::TrySendError;

#[test]
fn model_command_channel_is_bounded_and_non_blocking() {
    let (sender, _receiver) = command_channel();
    for index in 0..COMMAND_CHANNEL_CAPACITY {
        sender
            .try_send(ModelCommand::PreviewMotion(format!("Motion{index}")))
            .expect("channel should accept commands up to its capacity");
    }

    assert!(matches!(
        sender.try_send(ModelCommand::PreviewMotion("Overflow".to_owned())),
        Err(TrySendError::Full(_))
    ));
}
