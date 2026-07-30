use std::time::Duration;

use super::super::{SpeechPlayback, playback::resample_for_test};

#[test]
fn matching_sample_rate_keeps_pcm_unchanged() {
    let samples = vec![-1.0, -0.5, 0.0, 0.5, 1.0];

    assert_eq!(
        resample_for_test(samples.clone(), 24_000, 24_000).expect("同采样率应直接返回输入"),
        samples
    );
}

#[test]
fn resampling_to_48khz_produces_finite_double_length_audio() {
    let samples = (0..2_400)
        .map(|index| ((index as f32) / 20.0).sin() * 0.5)
        .collect::<Vec<_>>();

    let output = resample_for_test(samples, 24_000, 48_000).expect("24 kHz PCM 应可重采样");

    assert!(
        (4_798..=4_802).contains(&output.len()),
        "长度：{}",
        output.len()
    );
    assert!(output.iter().all(|sample| sample.is_finite()));
}

#[test]
fn idle_playback_thread_shuts_down_within_the_bound() {
    let (_playback, shutdown) = SpeechPlayback::start().expect("空闲播放线程应可启动");

    assert!(shutdown.shutdown(Duration::from_secs(1)));
}
