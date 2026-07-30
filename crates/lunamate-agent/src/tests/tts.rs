use crate::tts::{
    SpeechSynthesisError, decode_pcm_for_test, parse_volc_response_for_test, volc_message_for_test,
};

#[test]
fn volc_session_message_contains_event_session_and_payload() {
    let frame = volc_message_for_test(200, Some("session-1"), br#"{"text":"hello"}"#);

    assert_eq!(&frame[..4], &[0x11, 0x14, 0x10, 0]);
    assert_eq!(&frame[4..8], &200_u32.to_be_bytes());
    assert!(frame.windows(9).any(|window| window == b"session-1"));
}

#[test]
fn volc_parser_rejects_truncated_payloads() {
    let mut frame = vec![0x11, 0xb0, 0x10, 0];
    frame.extend_from_slice(&8_u32.to_be_bytes());
    frame.extend_from_slice(&[1, 2]);

    assert_eq!(
        parse_volc_response_for_test(&frame),
        Err(SpeechSynthesisError::InvalidResponse)
    );
}

#[test]
fn pcm_decoder_requires_nonempty_aligned_s16le() {
    assert_eq!(
        decode_pcm_for_test(Vec::new()),
        Err(SpeechSynthesisError::InvalidResponse)
    );
    assert_eq!(
        decode_pcm_for_test(vec![1]),
        Err(SpeechSynthesisError::InvalidResponse)
    );
    assert_eq!(
        decode_pcm_for_test(vec![0, 0, 0xff, 0x7f]),
        Ok(vec![0, i16::MAX])
    );
}
