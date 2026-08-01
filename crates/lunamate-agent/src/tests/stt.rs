use crate::config::AppLanguage;
use crate::stt::{
    TranscriptionError, encode_sauc_frame_for_test, encode_wav_for_test,
    parse_sauc_response_for_test,
};

#[test]
fn wav_encoder_writes_bounded_mono_pcm_header() {
    let wav = encode_wav_for_test(&[i16::MIN, 0, i16::MAX]).expect("有效 PCM 应可编码");

    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[8..12], b"WAVE");
    assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1);
    assert_eq!(
        u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
        16_000
    );
    assert_eq!(u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]), 6);
}

#[test]
fn sauc_audio_frames_use_current_flags_and_gzip_payload() {
    let frame = encode_sauc_frame_for_test(2, 1, 1, 1, 7, &[1, 2, 3]).expect("短音频帧应可编码");

    assert_eq!(&frame[..4], &[0x11, 0x21, 0x11, 0]);
    assert_eq!(&frame[4..8], &7_i32.to_be_bytes());
    let payload_len = u32::from_be_bytes(frame[8..12].try_into().expect("帧长度字段完整"));
    assert_eq!(payload_len as usize, frame.len() - 12);
    assert!(frame.len() > 12);
}

#[test]
fn sauc_final_audio_frame_has_negative_sequence() {
    let frame = encode_sauc_frame_for_test(2, 3, 1, 1, -8, &[]).expect("结束帧应可编码");

    assert_eq!(&frame[..4], &[0x11, 0x23, 0x11, 0]);
    assert_eq!(&frame[4..8], &(-8_i32).to_be_bytes());
    let payload_len = u32::from_be_bytes(frame[8..12].try_into().expect("帧长度字段完整"));
    assert!(payload_len > 0);
    assert_eq!(payload_len as usize, frame.len() - 12);
}

#[test]
fn sauc_parser_rejects_truncated_and_oversized_payloads() {
    assert_eq!(
        parse_sauc_response_for_test(&[0x11, 0x91]),
        Err(TranscriptionError::InvalidResponse)
    );
    let mut frame = vec![0x11, 0x90, 0x10, 0];
    frame.extend_from_slice(&(300_u32 * 1024).to_be_bytes());

    assert_eq!(
        parse_sauc_response_for_test(&frame),
        Err(TranscriptionError::ResponseTooLarge)
    );
}

#[test]
fn transcription_errors_use_the_explicit_request_language() {
    assert_eq!(
        TranscriptionError::Timeout.localized_message(AppLanguage::English),
        "The transcription request timed out"
    );
    assert_eq!(
        TranscriptionError::Timeout.localized_message(AppLanguage::Japanese),
        "文字起こし要求がタイムアウトしました"
    );
}
