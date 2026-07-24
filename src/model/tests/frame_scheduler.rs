use std::time::{Duration, Instant};

use crate::model::frame_scheduler::{
    FramePacer, FrameRateMeter, MIN_OVER_BUDGET_REST, OVERRUNS_BEFORE_DOWNSHIFT,
    RECOVERY_FRAMES_BEFORE_UPSHIFT, frame_wake_channel,
};

fn run_frame(
    pacer: &mut FramePacer,
    now: &mut Instant,
    elapsed: Duration,
    wake_overshoot: Duration,
    tail_work: Duration,
) -> Instant {
    let delay = pacer.delay_until_next_frame(*now);
    let frame_started = *now + delay + wake_overshoot;
    let completed_at = frame_started + elapsed;
    pacer.complete_frame(frame_started, completed_at);
    *now = completed_at + tail_work;
    frame_started
}

#[test]
fn frame_rate_meter_reports_measured_rate_and_decays_after_idle() {
    let started = Instant::now();
    let mut meter = FrameRateMeter::new();
    for frame in 0_u64..=30 {
        meter.record(started + Duration::from_millis(frame * 16));
    }

    let measured = meter.sample(started + Duration::from_millis(480));
    assert!((measured - 62.5).abs() < 0.1, "实际测量值为 {measured}");
    assert_eq!(meter.sample(started + Duration::from_secs(2)), 0.0);
}

#[test]
fn frame_rate_meter_resets_between_model_generations() {
    let started = Instant::now();
    let mut meter = FrameRateMeter::new();
    meter.record(started);
    meter.record(started + Duration::from_millis(16));
    assert!(meter.sample(started + Duration::from_millis(16)) > 0.0);

    meter.reset();
    assert_eq!(meter.sample(started + Duration::from_millis(16)), 0.0);
}

#[test]
fn frame_rate_meter_preserves_frames_hidden_by_coalesced_notifications() {
    let started = Instant::now();
    let mut meter = FrameRateMeter::new();
    meter.record_cumulative(started, 1);
    meter.record_cumulative(started + Duration::from_millis(480), 31);

    let measured = meter.sample(started + Duration::from_millis(480));
    assert!((measured - 62.5).abs() < 0.1, "实际测量值为 {measured}");
}

#[test]
fn cumulative_rate_keeps_an_anchor_across_a_long_ui_delay() {
    let started = Instant::now();
    let mut meter = FrameRateMeter::new();
    meter.record_cumulative(started, 1);
    meter.record_cumulative(started + Duration::from_millis(1_200), 73);

    let measured = meter.sample(started + Duration::from_millis(1_200));
    assert!((measured - 60.0).abs() < 0.1, "实际测量值为 {measured}");
}

#[test]
fn frame_rate_meter_discards_an_idle_anchor_before_resuming() {
    let started = Instant::now();
    let mut meter = FrameRateMeter::new();
    meter.record_cumulative(started, 1);
    meter.record_cumulative(started + Duration::from_millis(16), 2);

    let idle = started + Duration::from_secs(2);
    meter.record_cumulative(idle, 3);
    assert_eq!(meter.sample(idle), 0.0);
    meter.record_cumulative(idle + Duration::from_millis(16), 4);

    let measured = meter.sample(idle + Duration::from_millis(16));
    assert!((measured - 62.5).abs() < 0.1, "实际测量值为 {measured}");
}

#[test]
fn standard_rates_precompute_expected_degradation_tiers() {
    assert_eq!(FramePacer::new(Some(30), true).tier_fps(), vec![30, 15, 10]);
    assert_eq!(FramePacer::new(Some(60), true).tier_fps(), vec![60, 30, 15]);
    assert_eq!(
        FramePacer::new(Some(120), true).tier_fps(),
        vec![120, 60, 30]
    );
}

#[test]
fn arbitrary_adaptive_targets_precompute_and_deduplicate_tiers() {
    assert_eq!(FramePacer::new(Some(75), true).tier_fps(), vec![75, 38, 19]);
    assert_eq!(FramePacer::new(Some(24), true).tier_fps(), vec![24, 15, 10]);
    assert_eq!(FramePacer::new(Some(10), true).tier_fps(), vec![10]);
    assert_eq!(
        FramePacer::new(Some(u16::MAX), true).tier_fps(),
        vec![u16::MAX, 32_768, 16_384]
    );
}

#[test]
fn unlimited_rate_skips_budget_delays_and_degradation() {
    let mut pacer = FramePacer::new(None, false);
    let mut now = Instant::now();

    assert_eq!(pacer.delay_until_next_frame(now), Duration::ZERO);
    for _ in 0..OVERRUNS_BEFORE_DOWNSHIFT * 2 {
        run_frame(
            &mut pacer,
            &mut now,
            Duration::from_secs(1),
            Duration::ZERO,
            Duration::ZERO,
        );
        assert_eq!(pacer.delay_until_next_frame(now), Duration::ZERO);
    }
    assert!(pacer.tier_fps().is_empty());
}

#[test]
fn repeated_overruns_downshift_one_tier_at_a_time() {
    let mut pacer = FramePacer::new(Some(60), true);
    let mut now = Instant::now();
    let over_budget = Duration::from_millis(20);

    for _ in 0..OVERRUNS_BEFORE_DOWNSHIFT - 1 {
        run_frame(
            &mut pacer,
            &mut now,
            over_budget,
            Duration::ZERO,
            Duration::ZERO,
        );
        assert_eq!(pacer.delay_until_next_frame(now), MIN_OVER_BUDGET_REST);
        assert_eq!(pacer.current_fps(), 60);
    }
    run_frame(
        &mut pacer,
        &mut now,
        over_budget,
        Duration::ZERO,
        Duration::ZERO,
    );
    assert!(pacer.delay_until_next_frame(now) > Duration::ZERO);
    assert_eq!(pacer.current_fps(), 30);

    for _ in 0..OVERRUNS_BEFORE_DOWNSHIFT {
        run_frame(
            &mut pacer,
            &mut now,
            Duration::from_millis(40),
            Duration::ZERO,
            Duration::ZERO,
        );
    }
    assert_eq!(pacer.current_fps(), 15);
}

#[test]
fn strict_custom_rate_never_degrades_to_half_the_target() {
    let mut pacer = FramePacer::new(Some(240), false);
    let mut now = Instant::now();
    assert_eq!(pacer.tier_fps(), vec![240]);

    for _ in 0..OVERRUNS_BEFORE_DOWNSHIFT * 3 {
        run_frame(
            &mut pacer,
            &mut now,
            Duration::from_millis(5),
            Duration::ZERO,
            Duration::ZERO,
        );
        assert_eq!(pacer.current_fps(), 240);
        assert_eq!(pacer.delay_until_next_frame(now), Duration::ZERO);
    }
}

#[test]
fn strict_custom_rate_can_hold_a_target_above_120() {
    let mut pacer = FramePacer::new(Some(240), false);
    let mut now = Instant::now();
    let first_frame = run_frame(
        &mut pacer,
        &mut now,
        Duration::from_millis(1),
        Duration::from_micros(500),
        Duration::from_micros(250),
    );
    let mut last_frame = first_frame;
    for _ in 0..240 {
        last_frame = run_frame(
            &mut pacer,
            &mut now,
            Duration::from_millis(1),
            Duration::from_micros(500),
            Duration::from_micros(250),
        );
    }

    let measured = 240.0 / last_frame.duration_since(first_frame).as_secs_f64();
    assert!(
        (measured - 240.0).abs() < 0.01,
        "严格自定义节拍下的实际测量值为 {measured}"
    );
}

#[test]
fn sustained_headroom_recovers_without_oscillating() {
    let mut pacer = FramePacer::new(Some(60), true);
    let mut now = Instant::now();
    for _ in 0..OVERRUNS_BEFORE_DOWNSHIFT {
        run_frame(
            &mut pacer,
            &mut now,
            Duration::from_millis(20),
            Duration::ZERO,
            Duration::ZERO,
        );
    }
    assert_eq!(pacer.current_fps(), 30);

    for _ in 0..RECOVERY_FRAMES_BEFORE_UPSHIFT - 1 {
        run_frame(
            &mut pacer,
            &mut now,
            Duration::from_millis(10),
            Duration::ZERO,
            Duration::ZERO,
        );
        assert_eq!(pacer.current_fps(), 30);
    }
    run_frame(
        &mut pacer,
        &mut now,
        Duration::from_millis(10),
        Duration::ZERO,
        Duration::ZERO,
    );
    assert_eq!(pacer.current_fps(), 60);
}

#[test]
fn idle_reset_restores_configured_tier() {
    let mut pacer = FramePacer::new(Some(120), true);
    let mut now = Instant::now();
    for _ in 0..OVERRUNS_BEFORE_DOWNSHIFT {
        run_frame(
            &mut pacer,
            &mut now,
            Duration::from_millis(10),
            Duration::ZERO,
            Duration::ZERO,
        );
    }
    assert_eq!(pacer.current_fps(), 60);

    pacer.reset_after_idle();
    assert_eq!(pacer.current_fps(), 120);
    assert_eq!(
        pacer.delay_until_next_frame(now),
        Duration::from_secs_f64(1.0 / 120.0)
    );
}

#[test]
fn changed_target_rebuilds_tiers_immediately() {
    let mut pacer = FramePacer::new(Some(30), true);
    pacer.set_target_fps(Some(60), true);

    assert_eq!(pacer.tier_fps(), vec![60, 30, 15]);
    assert_eq!(pacer.current_fps(), 60);
}

#[test]
fn changing_to_strict_pacing_rebuilds_the_same_numeric_target() {
    let mut pacer = FramePacer::new(Some(240), true);
    let mut now = Instant::now();
    for _ in 0..OVERRUNS_BEFORE_DOWNSHIFT {
        run_frame(
            &mut pacer,
            &mut now,
            Duration::from_millis(5),
            Duration::ZERO,
            Duration::ZERO,
        );
    }
    assert_eq!(pacer.current_fps(), 120);

    pacer.set_target_fps(Some(240), false);
    assert_eq!(pacer.current_fps(), 240);
    assert_eq!(pacer.tier_fps(), vec![240]);
}

#[test]
fn changing_to_and_from_unlimited_rebuilds_scheduler() {
    let mut pacer = FramePacer::new(Some(60), true);
    let now = Instant::now();
    pacer.set_target_fps(None, false);
    assert_eq!(pacer.delay_until_next_frame(now), Duration::ZERO);
    assert!(pacer.tier_fps().is_empty());

    pacer.set_target_fps(Some(30), true);
    assert_eq!(pacer.tier_fps(), vec![30, 15, 10]);
    assert_eq!(pacer.current_fps(), 30);
}

#[test]
fn absolute_deadlines_prevent_timer_overshoot_from_accumulating() {
    let mut pacer = FramePacer::new(Some(120), true);
    let mut now = Instant::now();
    let render_time = Duration::from_millis(2);
    let wake_overshoot = Duration::from_millis(1);
    let tail_work = Duration::from_micros(500);
    let first_frame = run_frame(&mut pacer, &mut now, render_time, wake_overshoot, tail_work);
    let mut last_frame = first_frame;
    for _ in 0..120 {
        last_frame = run_frame(&mut pacer, &mut now, render_time, wake_overshoot, tail_work);
    }

    let measured = 120.0 / last_frame.duration_since(first_frame).as_secs_f64();
    assert!(
        (measured - 120.0).abs() < 0.01,
        "绝对节拍下的实际测量值为 {measured}"
    );
}

#[test]
fn postponed_frame_is_honored_even_without_a_rate_limit() {
    let mut pacer = FramePacer::new(None, false);
    let now = Instant::now();
    let retry_delay = Duration::from_millis(16);
    pacer.postpone_next_frame(now, retry_delay);

    assert_eq!(pacer.delay_until_next_frame(now), retry_delay);
}

#[test]
fn wake_signals_are_coalesced() {
    let (wake, receiver) = frame_wake_channel();
    wake.wake();
    wake.wake();

    assert!(receiver.drain());
    assert!(!receiver.drain());
}
