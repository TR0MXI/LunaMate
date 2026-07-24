//! 根据配置帧率维护绝对帧时刻与降载档位，并为静止模型提供可合并的异步唤醒。

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use async_channel::{Receiver, Sender, TryRecvError, TrySendError};

const MAX_FRAME_TIERS: usize = 3;
const MIDDLE_TIER_FLOOR_FPS: u16 = 15;
const LOW_TIER_FLOOR_FPS: u16 = 10;
const OVERRUNS_BEFORE_DOWNSHIFT: u8 = 3;
const RECOVERY_FRAMES_BEFORE_UPSHIFT: u8 = 30;
const RECOVERY_HEADROOM: f32 = 0.8;
const MIN_OVER_BUDGET_REST: Duration = Duration::from_millis(5);
const FRAME_RATE_SAMPLE_WINDOW: Duration = Duration::from_secs(1);

/// 在一秒滑动窗口内统计实际完成的模型帧率。
#[derive(Default)]
pub(crate) struct FrameRateMeter {
    samples: VecDeque<FrameRateSample>,
    total_frames: u64,
}

#[derive(Clone, Copy)]
struct FrameRateSample {
    recorded_at: Instant,
    total_frames: u64,
}

impl FrameRateMeter {
    /// 创建尚未收到任何帧样本的计数器。
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 记录一帧完成时间，并丢弃滑动窗口外的旧样本。
    pub(crate) fn record(&mut self, now: Instant) {
        self.record_cumulative(now, self.total_frames.saturating_add(1));
    }

    /// 记录渲染端累计完成帧数，使合并通知仍能保留期间全部帧。
    pub(crate) fn record_cumulative(&mut self, now: Instant, total_frames: u64) {
        if self
            .samples
            .back()
            .is_some_and(|last| last.recorded_at > now || last.total_frames > total_frames)
        {
            self.reset();
        }
        if self
            .samples
            .back()
            .is_some_and(|last| last.total_frames == total_frames)
        {
            return;
        }
        if self.samples.back().is_some_and(|last| {
            now.saturating_duration_since(last.recorded_at) >= FRAME_RATE_SAMPLE_WINDOW
                && total_frames.saturating_sub(last.total_frames) == 1
        }) {
            self.samples.clear();
        }
        self.total_frames = total_frames;
        self.samples.push_back(FrameRateSample {
            recorded_at: now,
            total_frames,
        });
        self.prune(now);
    }

    /// 返回当前滑动窗口内测得的每秒帧数；样本不足或已空闲时返回零。
    pub(crate) fn sample(&mut self, now: Instant) -> f32 {
        self.prune(now);
        let Some(last) = self.samples.back().copied() else {
            return 0.0;
        };
        if now.saturating_duration_since(last.recorded_at) >= FRAME_RATE_SAMPLE_WINDOW {
            self.samples.clear();
            return 0.0;
        }
        let Some(first) = self.samples.front().copied() else {
            return 0.0;
        };
        if self.samples.len() < 2 {
            return 0.0;
        }
        let elapsed = last
            .recorded_at
            .saturating_duration_since(first.recorded_at)
            .as_secs_f32();
        if elapsed <= f32::EPSILON {
            return 0.0;
        }
        (last.total_frames.saturating_sub(first.total_frames) as f32 / elapsed).max(0.0)
    }

    /// 清除切换模型或关闭显示前累积的样本。
    pub(crate) fn reset(&mut self) {
        self.samples.clear();
        self.total_frames = 0;
    }

    fn prune(&mut self, now: Instant) {
        // 累计计数需要保留窗口边界前的最近锚点，否则一次超过一秒的 UI 延迟会
        // 同时删除起止端点，把持续 present 错报为零帧。
        while self.samples.len() > 1
            && self.samples.get(1).is_some_and(|sample| {
                now.saturating_duration_since(sample.recorded_at) >= FRAME_RATE_SAMPLE_WINDOW
            })
        {
            self.samples.pop_front();
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct FrameTier {
    fps: u16,
    interval: Duration,
}

impl FrameTier {
    fn new(fps: u16) -> Self {
        Self {
            fps,
            interval: Duration::from_secs_f64(1.0 / f64::from(fps)),
        }
    }
}

/// 跟踪绝对帧时刻与当前帧率档位，并通过滞回避免超预算边界附近频繁切换。
pub(crate) struct FramePacer {
    target_fps: Option<u16>,
    tiers: [FrameTier; MAX_FRAME_TIERS],
    tier_count: usize,
    current_tier: usize,
    consecutive_overruns: u8,
    consecutive_recovery_frames: u8,
    next_frame_at: Option<Instant>,
}

impl FramePacer {
    /// 根据可选目标帧率预计算最多三个由快到慢的唯一档位。
    ///
    /// `None` 表示无限制模式，不设置帧预算，也不触发超预算降档。
    pub(crate) fn new(target_fps: Option<u16>) -> Self {
        let Some(target_fps) = target_fps else {
            return Self {
                target_fps: None,
                tiers: [FrameTier::new(1); MAX_FRAME_TIERS],
                tier_count: 0,
                current_tier: 0,
                consecutive_overruns: 0,
                consecutive_recovery_frames: 0,
                next_frame_at: None,
            };
        };
        let middle_floor = MIDDLE_TIER_FLOOR_FPS.min(target_fps);
        let low_floor = LOW_TIER_FLOOR_FPS.min(target_fps);
        let candidates = [
            target_fps,
            rounded_div(target_fps, 2).max(middle_floor),
            rounded_div(target_fps, 4).max(low_floor),
        ];
        let mut tiers = [FrameTier::new(target_fps); MAX_FRAME_TIERS];
        let mut tier_count = 0;
        for fps in candidates {
            if tiers[..tier_count].iter().any(|tier| tier.fps == fps) {
                continue;
            }
            tiers[tier_count] = FrameTier::new(fps);
            tier_count += 1;
        }

        Self {
            target_fps: Some(target_fps),
            tiers,
            tier_count,
            current_tier: 0,
            consecutive_overruns: 0,
            consecutive_recovery_frames: 0,
            next_frame_at: None,
        }
    }

    /// 返回距离下一绝对帧时刻的剩余时间；首次调用从当前时刻建立节拍。
    pub(crate) fn delay_until_next_frame(&mut self, now: Instant) -> Duration {
        if let Some(next_frame_at) = self.next_frame_at {
            return next_frame_at.saturating_duration_since(now);
        }
        let Some(current) = self.current() else {
            return Duration::ZERO;
        };
        self.next_frame_at = Some(now + current.interval);
        current.interval
    }

    /// 在 UI 热更新帧率后重建调度档位；目标未变化时保留当前滞回状态。
    pub(crate) fn set_target_fps(&mut self, target_fps: Option<u16>) {
        if self.target_fps != target_fps {
            *self = Self::new(target_fps);
        }
    }

    /// 记录一帧的完整耗时，并沿既有绝对节拍推进下一帧时刻。
    ///
    /// 定时器超时和帧尾处理开销不会被重复加到每个帧间隔中；错过当前节拍时
    /// 最多立即补一帧，真实渲染超预算时仍保留最小休息以避免忙循环。
    pub(crate) fn complete_frame(&mut self, frame_started: Instant, completed_at: Instant) {
        let elapsed = completed_at.saturating_duration_since(frame_started);
        self.record_frame_duration(elapsed);
        let Some(current) = self.current() else {
            self.next_frame_at = None;
            return;
        };
        let cadence_anchor = self.next_frame_at.take().unwrap_or(frame_started);
        let cadence_deadline = cadence_anchor + current.interval;
        let earliest_deadline = if elapsed >= current.interval {
            completed_at + MIN_OVER_BUDGET_REST
        } else {
            completed_at
        };
        self.next_frame_at = Some(cadence_deadline.max(earliest_deadline));
    }

    /// 将下一帧至少推迟指定时长，用于 surface 暂时不可用等独立于帧率的退避。
    pub(crate) fn postpone_next_frame(&mut self, now: Instant, minimum_delay: Duration) {
        let not_before = now + minimum_delay;
        self.next_frame_at = Some(
            self.next_frame_at
                .map_or(not_before, |deadline| deadline.max(not_before)),
        );
    }

    fn record_frame_duration(&mut self, elapsed: Duration) {
        let Some(current) = self.current() else {
            return;
        };
        if elapsed >= current.interval {
            self.consecutive_overruns = self.consecutive_overruns.saturating_add(1);
            self.consecutive_recovery_frames = 0;
            if self.consecutive_overruns >= OVERRUNS_BEFORE_DOWNSHIFT
                && self.current_tier + 1 < self.tier_count
            {
                self.current_tier += 1;
                self.consecutive_overruns = 0;
            }
        } else {
            self.consecutive_overruns = 0;
            if self.has_recovery_headroom(elapsed) {
                self.consecutive_recovery_frames =
                    self.consecutive_recovery_frames.saturating_add(1);
                if self.consecutive_recovery_frames >= RECOVERY_FRAMES_BEFORE_UPSHIFT {
                    self.current_tier -= 1;
                    self.consecutive_recovery_frames = 0;
                }
            } else {
                self.consecutive_recovery_frames = 0;
            }
        }
    }

    /// 静止期间没有持续负载，唤醒后从用户配置的最高档重新评估。
    pub(crate) fn reset_after_idle(&mut self) {
        self.current_tier = 0;
        self.consecutive_overruns = 0;
        self.consecutive_recovery_frames = 0;
        self.next_frame_at = None;
    }

    fn current(&self) -> Option<FrameTier> {
        (self.current_tier < self.tier_count).then(|| self.tiers[self.current_tier])
    }

    fn has_recovery_headroom(&self, elapsed: Duration) -> bool {
        if self.current_tier == 0 {
            return false;
        }
        elapsed
            <= self.tiers[self.current_tier - 1]
                .interval
                .mul_f32(RECOVERY_HEADROOM)
    }

    #[cfg(test)]
    fn tier_fps(&self) -> Vec<u16> {
        self.tiers[..self.tier_count]
            .iter()
            .map(|tier| tier.fps)
            .collect()
    }

    #[cfg(test)]
    fn current_fps(&self) -> u16 {
        self.current()
            .map(|tier| tier.fps)
            .expect("有限帧率测试必须存在当前降载档位")
    }
}

fn rounded_div(value: u16, divisor: u16) -> u16 {
    value.saturating_add(divisor / 2) / divisor
}

/// UI 侧持有的合并唤醒端；已有未消费信号时重复唤醒不会扩张队列。
pub(crate) struct FrameWake {
    sender: Sender<()>,
}

impl FrameWake {
    /// 请求后台模型处理最新输入；关闭或已有信号都无需调用方处理。
    pub(crate) fn wake(&self) {
        match self.sender.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) | Err(TrySendError::Closed(())) => {}
        }
    }

    /// 关闭当前 generation 的等待端，使静止任务可以立即退出。
    pub(crate) fn close(&self) {
        self.sender.close();
    }
}

/// 后台模型侧持有的单消费者等待端。
pub(crate) struct FrameWakeReceiver {
    receiver: Receiver<()>,
}

impl FrameWakeReceiver {
    /// 等待至少一个输入信号；发送端关闭时返回 `false`。
    pub(crate) async fn wait(&self) -> bool {
        self.receiver.recv().await.is_ok()
    }

    /// 清除当前已经合并的信号，并返回是否观察到待处理输入。
    pub(crate) fn drain(&self) -> bool {
        match self.receiver.try_recv() {
            Ok(()) => true,
            Err(TryRecvError::Empty | TryRecvError::Closed) => false,
        }
    }
}

/// 创建容量为一的 generation 专用唤醒通道。
pub(crate) fn frame_wake_channel() -> (FrameWake, FrameWakeReceiver) {
    let (sender, receiver) = async_channel::bounded(1);
    (FrameWake { sender }, FrameWakeReceiver { receiver })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(FramePacer::new(Some(30)).tier_fps(), vec![30, 15, 10]);
        assert_eq!(FramePacer::new(Some(60)).tier_fps(), vec![60, 30, 15]);
        assert_eq!(FramePacer::new(Some(120)).tier_fps(), vec![120, 60, 30]);
    }

    #[test]
    fn custom_rates_precompute_and_deduplicate_tiers() {
        assert_eq!(FramePacer::new(Some(75)).tier_fps(), vec![75, 38, 19]);
        assert_eq!(FramePacer::new(Some(24)).tier_fps(), vec![24, 15, 10]);
        assert_eq!(FramePacer::new(Some(10)).tier_fps(), vec![10]);
    }

    #[test]
    fn unlimited_rate_skips_budget_delays_and_degradation() {
        let mut pacer = FramePacer::new(None);
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
        let mut pacer = FramePacer::new(Some(60));
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
    fn sustained_headroom_recovers_without_oscillating() {
        let mut pacer = FramePacer::new(Some(60));
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
        let mut pacer = FramePacer::new(Some(120));
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
        let mut pacer = FramePacer::new(Some(30));
        pacer.set_target_fps(Some(60));

        assert_eq!(pacer.tier_fps(), vec![60, 30, 15]);
        assert_eq!(pacer.current_fps(), 60);
    }

    #[test]
    fn changing_to_and_from_unlimited_rebuilds_scheduler() {
        let mut pacer = FramePacer::new(Some(60));
        let now = Instant::now();
        pacer.set_target_fps(None);
        assert_eq!(pacer.delay_until_next_frame(now), Duration::ZERO);
        assert!(pacer.tier_fps().is_empty());

        pacer.set_target_fps(Some(30));
        assert_eq!(pacer.tier_fps(), vec![30, 15, 10]);
        assert_eq!(pacer.current_fps(), 30);
    }

    #[test]
    fn absolute_deadlines_prevent_timer_overshoot_from_accumulating() {
        let mut pacer = FramePacer::new(Some(120));
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
        let mut pacer = FramePacer::new(None);
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
}
