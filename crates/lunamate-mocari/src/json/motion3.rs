use serde::Deserialize;

use crate::{Error, Result};

const FORMAT: &str = "motion3.json";
const SUPPORTED_VERSIONS: [u32; 2] = [0, 3];
const MAX_MOTION_CURVES: usize = 4_096;
const MAX_SEGMENTS_PER_CURVE: usize = 16_384;
const MAX_TOTAL_SEGMENTS: usize = 65_536;
const MAX_TOTAL_POINTS: usize = 131_072;

#[derive(Debug, Clone, PartialEq)]
/// Parsed Cubism `motion3.json` data.
pub struct Motion3 {
    version: u32,
    meta: MotionMeta,
    curves: Vec<MotionCurve>,
}

impl Motion3 {
    /// Parses a motion JSON document from a string.
    pub fn from_json_str(source: &str) -> Result<Self> {
        let raw: RawMotion3 = serde_json::from_str(source).map_err(|error| Error::InvalidJson {
            format: FORMAT,
            message: error.to_string(),
        })?;

        // VTube Studio motion recording exports use the Cubism 3 layout with Version 0.
        if !SUPPORTED_VERSIONS.contains(&raw.version) {
            return Err(Error::UnsupportedVersion {
                format: FORMAT,
                version: raw.version,
            });
        }

        validate_finite(raw.meta.duration, "motion duration")?;
        validate_finite(raw.meta.fps, "motion FPS")?;

        let actual_curve_count = raw.curves.len();
        if actual_curve_count > MAX_MOTION_CURVES {
            return Err(invalid_motion(format!(
                "curve count {actual_curve_count} exceeds limit {MAX_MOTION_CURVES}"
            )));
        }

        let are_beziers_restricted = raw.meta.are_beziers_restricted;
        let mut curves = Vec::new();
        curves
            .try_reserve_exact(actual_curve_count)
            .map_err(|error| invalid_motion(format!("failed to reserve motion curves: {error}")))?;
        let mut actual_segment_count = 0_usize;
        let mut actual_point_count = 0_usize;

        for (curve_index, raw_curve) in raw.curves.into_iter().enumerate() {
            let (curve, point_count) =
                MotionCurve::from_raw(raw_curve, are_beziers_restricted, curve_index)?;
            actual_segment_count = actual_segment_count
                .checked_add(curve.segments.len())
                .ok_or_else(|| invalid_motion("total segment count overflows"))?;
            if actual_segment_count > MAX_TOTAL_SEGMENTS {
                return Err(invalid_motion(format!(
                    "total segment count {actual_segment_count} exceeds limit {MAX_TOTAL_SEGMENTS}"
                )));
            }

            actual_point_count = actual_point_count
                .checked_add(point_count)
                .ok_or_else(|| invalid_motion("total point count overflows"))?;
            if actual_point_count > MAX_TOTAL_POINTS {
                return Err(invalid_motion(format!(
                    "total point count {actual_point_count} exceeds limit {MAX_TOTAL_POINTS}"
                )));
            }
            curves.push(curve);
        }

        validate_reported_counts(
            raw.version,
            &raw.meta,
            actual_curve_count,
            actual_segment_count,
            actual_point_count,
        )?;

        Ok(Self {
            version: raw.version,
            meta: raw.meta,
            curves,
        })
    }

    /// Returns the parsed motion format version.
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Returns motion metadata such as duration, FPS, and loop flag.
    pub fn meta(&self) -> &MotionMeta {
        &self.meta
    }

    /// Returns all animation curves in this motion.
    pub fn curves(&self) -> &[MotionCurve] {
        &self.curves
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
/// Metadata from a Cubism motion file.
pub struct MotionMeta {
    #[serde(rename = "Duration")]
    duration: f32,
    #[serde(rename = "Fps")]
    fps: f32,
    #[serde(rename = "Loop")]
    loop_motion: bool,
    #[serde(rename = "AreBeziersRestricted", default)]
    are_beziers_restricted: bool,
    #[serde(rename = "CurveCount", default)]
    curve_count: u32,
    #[serde(rename = "TotalSegmentCount", default)]
    total_segment_count: u32,
    #[serde(rename = "TotalPointCount", default)]
    total_point_count: u32,
    #[serde(rename = "UserDataCount", default)]
    user_data_count: u32,
    #[serde(rename = "TotalUserDataSize", default)]
    total_user_data_size: u32,
}

impl MotionMeta {
    /// Returns the motion duration in seconds.
    pub fn duration(&self) -> f32 {
        self.duration
    }

    /// Returns the authoring frames per second value.
    pub fn fps(&self) -> f32 {
        self.fps
    }

    /// Returns whether the motion should loop.
    pub fn is_looping(&self) -> bool {
        self.loop_motion
    }

    /// Returns whether Bezier control points use restricted Cubism semantics.
    pub fn are_beziers_restricted(&self) -> bool {
        self.are_beziers_restricted
    }

    /// Returns the curve count reported by the file metadata.
    pub fn curve_count(&self) -> u32 {
        self.curve_count
    }

    /// Returns the total segment count reported by the file metadata.
    pub fn total_segment_count(&self) -> u32 {
        self.total_segment_count
    }

    /// Returns the total point count reported by the file metadata.
    pub fn total_point_count(&self) -> u32 {
        self.total_point_count
    }

    /// Returns the user-data count reported by the file metadata.
    pub fn user_data_count(&self) -> u32 {
        self.user_data_count
    }

    /// Returns the total user-data byte size reported by the file metadata.
    pub fn total_user_data_size(&self) -> u32 {
        self.total_user_data_size
    }
}

#[derive(Debug, Clone, PartialEq)]
/// One animated target curve in a motion file.
pub struct MotionCurve {
    target: String,
    id: String,
    first_point: MotionPoint,
    segments: Vec<MotionSegment>,
    fade_in_time: Option<f32>,
    fade_out_time: Option<f32>,
    are_beziers_restricted: bool,
}

impl MotionCurve {
    /// Returns the curve target, such as `Parameter` or `PartOpacity`.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Returns the target parameter or part id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the first point in the curve.
    pub fn first_point(&self) -> MotionPoint {
        self.first_point
    }

    /// Returns all segments after the first point.
    pub fn segments(&self) -> &[MotionSegment] {
        &self.segments
    }

    /// Returns the optional per-curve fade-in override.
    pub fn fade_in_time(&self) -> Option<f32> {
        self.fade_in_time
    }

    /// Returns the optional per-curve fade-out override.
    pub fn fade_out_time(&self) -> Option<f32> {
        self.fade_out_time
    }

    /// Samples this curve at a time in seconds.
    pub fn sample(&self, time: f32) -> Option<f32> {
        if time <= self.first_point.time {
            return Some(self.first_point.value);
        }

        if time.is_nan() {
            return self
                .segments
                .last()
                .map(|segment| segment.end().value)
                .or(Some(self.first_point.value));
        }

        let segment_index = self
            .segments
            .partition_point(|segment| segment.end().time <= time);
        if let Some(segment) = self.segments.get(segment_index) {
            return segment.sample(time, self.are_beziers_restricted);
        }

        self.segments
            .last()
            .map(|segment| segment.end().value)
            .or(Some(self.first_point.value))
    }
}

impl MotionCurve {
    fn from_raw(
        raw: RawMotionCurve,
        are_beziers_restricted: bool,
        curve_index: usize,
    ) -> Result<(Self, usize)> {
        validate_optional_finite(
            raw.fade_in_time,
            &format!("curve {curve_index} fade-in time"),
        )?;
        validate_optional_finite(
            raw.fade_out_time,
            &format!("curve {curve_index} fade-out time"),
        )?;
        let (first_point, segments, point_count) = parse_segments(&raw.segments, curve_index)?;

        Ok((
            Self {
                target: raw.target,
                id: raw.id,
                first_point,
                segments,
                fade_in_time: raw.fade_in_time,
                fade_out_time: raw.fade_out_time,
                are_beziers_restricted,
            },
            point_count,
        ))
    }
}

/// Cubism sine easing used for fades.
pub fn easing_sine(value: f32) -> f32 {
    if value < 0.0 {
        return 0.0;
    }

    if value > 1.0 {
        return 1.0;
    }

    0.5 - 0.5 * (value * std::f32::consts::PI).cos()
}

/// Calculates a motion-level fade-in weight.
pub fn motion_fade_in_weight(
    user_time_seconds: f32,
    fade_in_start_time: f32,
    fade_in_seconds: f32,
) -> f32 {
    if fade_in_seconds <= 0.0 {
        1.0
    } else {
        easing_sine((user_time_seconds - fade_in_start_time) / fade_in_seconds)
    }
}

/// Calculates a motion-level fade-out weight.
pub fn motion_fade_out_weight(
    user_time_seconds: f32,
    end_time_seconds: f32,
    fade_out_seconds: f32,
) -> f32 {
    if fade_out_seconds <= 0.0 || end_time_seconds < 0.0 {
        1.0
    } else {
        easing_sine((end_time_seconds - user_time_seconds) / fade_out_seconds)
    }
}

#[allow(clippy::too_many_arguments)]
/// Combines motion-level and per-curve fade values into one curve weight.
pub fn parameter_curve_fade_weight(
    motion_weight: f32,
    motion_fade_in: f32,
    motion_fade_out: f32,
    curve_fade_in_seconds: Option<f32>,
    curve_fade_out_seconds: Option<f32>,
    user_time_seconds: f32,
    fade_in_start_time: f32,
    end_time_seconds: f32,
) -> f32 {
    // A negative per-curve fade time is the Cubism sentinel for "this curve has
    // no override; fall back to the motion-level fade", same as an absent value.
    let curve_fade_in_seconds = curve_fade_in_seconds.filter(|seconds| *seconds >= 0.0);
    let curve_fade_out_seconds = curve_fade_out_seconds.filter(|seconds| *seconds >= 0.0);

    if curve_fade_in_seconds.is_none() && curve_fade_out_seconds.is_none() {
        return motion_weight;
    }

    let fade_in = match curve_fade_in_seconds {
        Some(0.0) => 1.0,
        Some(seconds) => easing_sine((user_time_seconds - fade_in_start_time) / seconds),
        None => motion_fade_in,
    };
    let fade_out = match curve_fade_out_seconds {
        Some(0.0) => 1.0,
        Some(_) if end_time_seconds < 0.0 => 1.0,
        Some(seconds) => easing_sine((end_time_seconds - user_time_seconds) / seconds),
        None => motion_fade_out,
    };

    motion_weight * fade_in * fade_out
}

/// Blends a current value toward a sampled motion value.
pub fn apply_motion_fade(source_value: f32, target_value: f32, fade_weight: f32) -> f32 {
    source_value + (target_value - source_value) * fade_weight
}

#[derive(Debug, Copy, Clone, PartialEq)]
/// A point on a motion curve.
pub struct MotionPoint {
    /// Time in seconds.
    pub time: f32,
    /// Value at this point.
    pub value: f32,
}

#[derive(Debug, Copy, Clone, PartialEq)]
/// Segment interpolation mode between two motion points.
pub enum MotionSegment {
    /// Linear interpolation between start and end.
    Linear {
        /// Start point.
        start: MotionPoint,
        /// End point.
        end: MotionPoint,
    },
    /// Cubic Bezier interpolation.
    Bezier {
        /// Start point.
        start: MotionPoint,
        /// First control point.
        control1: MotionPoint,
        /// Second control point.
        control2: MotionPoint,
        /// End point.
        end: MotionPoint,
    },
    /// Holds the start value until the segment end.
    Stepped {
        /// Start point.
        start: MotionPoint,
        /// End point.
        end: MotionPoint,
    },
    /// Holds the end value for the segment.
    InverseStepped {
        /// Start point.
        start: MotionPoint,
        /// End point.
        end: MotionPoint,
    },
}

impl MotionSegment {
    /// Returns the segment end point.
    pub fn end(&self) -> MotionPoint {
        match *self {
            Self::Linear { end, .. }
            | Self::Bezier { end, .. }
            | Self::Stepped { end, .. }
            | Self::InverseStepped { end, .. } => end,
        }
    }

    /// Samples this segment at a time in seconds.
    pub fn sample(&self, time: f32, are_beziers_restricted: bool) -> Option<f32> {
        match *self {
            Self::Linear { start, end } => Some(sample_linear(start, end, time)),
            Self::Stepped { start, .. } => Some(start.value),
            Self::InverseStepped { end, .. } => Some(end.value),
            Self::Bezier {
                start,
                control1,
                control2,
                end,
            } => Some(sample_bezier(
                start,
                control1,
                control2,
                end,
                time,
                are_beziers_restricted,
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct RawMotion3 {
    #[serde(rename = "Version")]
    version: u32,
    #[serde(rename = "Meta")]
    meta: MotionMeta,
    #[serde(rename = "Curves", default)]
    curves: Vec<RawMotionCurve>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct RawMotionCurve {
    #[serde(rename = "Target")]
    target: String,
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Segments")]
    segments: Vec<f32>,
    #[serde(rename = "FadeInTime", default)]
    fade_in_time: Option<f32>,
    #[serde(rename = "FadeOutTime", default)]
    fade_out_time: Option<f32>,
}

fn parse_segments(
    values: &[f32],
    curve_index: usize,
) -> Result<(MotionPoint, Vec<MotionSegment>, usize)> {
    if values.len() < 2 {
        return Err(invalid_segments(
            "segments must start with a time/value point",
        ));
    }

    let first_point = MotionPoint {
        time: values[0],
        value: values[1],
    };
    validate_point(first_point, curve_index, None, "first point")?;
    let mut cursor = 2;
    let mut start = first_point;
    let mut segments = Vec::new();
    let estimated_segment_count = values
        .len()
        .saturating_sub(2)
        .div_ceil(3)
        .min(MAX_SEGMENTS_PER_CURVE);
    segments
        .try_reserve_exact(estimated_segment_count)
        .map_err(|error| {
            invalid_segments(format!(
                "curve {curve_index} failed to reserve segments: {error}"
            ))
        })?;
    let mut point_count = 1_usize;

    while cursor < values.len() {
        let segment_index = segments.len();
        if segment_index >= MAX_SEGMENTS_PER_CURVE {
            return Err(invalid_segments(format!(
                "curve {curve_index} segment count exceeds per-curve limit {MAX_SEGMENTS_PER_CURVE}"
            )));
        }

        let segment_type = segment_type(values[cursor])?;
        cursor += 1;

        let (segment, added_point_count) = match segment_type {
            0 => {
                let end = read_point(values, &mut cursor)?;
                validate_point(end, curve_index, Some(segment_index), "end point")?;
                (MotionSegment::Linear { start, end }, 1)
            }
            1 => {
                let control1 = read_point(values, &mut cursor)?;
                let control2 = read_point(values, &mut cursor)?;
                let end = read_point(values, &mut cursor)?;
                validate_point(
                    control1,
                    curve_index,
                    Some(segment_index),
                    "Bezier control point 1",
                )?;
                validate_point(
                    control2,
                    curve_index,
                    Some(segment_index),
                    "Bezier control point 2",
                )?;
                validate_point(end, curve_index, Some(segment_index), "end point")?;
                if control1.time < start.time
                    || control2.time < control1.time
                    || control2.time > end.time
                {
                    return Err(invalid_segments(format!(
                        "curve {curve_index} segment {segment_index} Bezier control point times must satisfy start <= control1 <= control2 <= end"
                    )));
                }
                (
                    MotionSegment::Bezier {
                        start,
                        control1,
                        control2,
                        end,
                    },
                    3,
                )
            }
            2 => {
                let end = read_point(values, &mut cursor)?;
                validate_point(end, curve_index, Some(segment_index), "end point")?;
                (MotionSegment::Stepped { start, end }, 1)
            }
            3 => {
                let end = read_point(values, &mut cursor)?;
                validate_point(end, curve_index, Some(segment_index), "end point")?;
                (MotionSegment::InverseStepped { start, end }, 1)
            }
            _ => return Err(invalid_segments("unsupported segment type")),
        };

        if segment.end().time <= start.time {
            return Err(invalid_segments(format!(
                "curve {curve_index} segment {segment_index} end time must be greater than its start time"
            )));
        }
        point_count = point_count
            .checked_add(added_point_count)
            .ok_or_else(|| invalid_segments("curve point count overflows"))?;
        start = segment.end();
        segments.push(segment);
    }

    Ok((first_point, segments, point_count))
}

fn read_point(values: &[f32], cursor: &mut usize) -> Result<MotionPoint> {
    if values.len().saturating_sub(*cursor) < 2 {
        return Err(invalid_segments("segment point is incomplete"));
    }

    let point = MotionPoint {
        time: values[*cursor],
        value: values[*cursor + 1],
    };
    *cursor += 2;
    Ok(point)
}

fn segment_type(value: f32) -> Result<u32> {
    if !value.is_finite() || value.fract() != 0.0 || !(0.0..=3.0).contains(&value) {
        return Err(invalid_segments("segment type must be 0, 1, 2, or 3"));
    }

    Ok(value as u32)
}

fn sample_linear(start: MotionPoint, end: MotionPoint, time: f32) -> f32 {
    if start.time == end.time {
        return end.value;
    }

    let amount = ((time - start.time) / (end.time - start.time)).max(0.0);
    start.value + (end.value - start.value) * amount
}

fn sample_bezier(
    start: MotionPoint,
    control1: MotionPoint,
    control2: MotionPoint,
    end: MotionPoint,
    time: f32,
    are_beziers_restricted: bool,
) -> f32 {
    let t = if are_beziers_restricted {
        if start.time == end.time {
            1.0
        } else {
            ((time - start.time) / (end.time - start.time)).max(0.0)
        }
    } else {
        solve_bezier_time(start, control1, control2, end, time)
    };

    cubic_bezier_point(start, control1, control2, end, t).value
}

fn cubic_bezier_point(
    start: MotionPoint,
    control1: MotionPoint,
    control2: MotionPoint,
    end: MotionPoint,
    t: f32,
) -> MotionPoint {
    let p01 = lerp_point(start, control1, t);
    let p12 = lerp_point(control1, control2, t);
    let p23 = lerp_point(control2, end, t);
    let p012 = lerp_point(p01, p12, t);
    let p123 = lerp_point(p12, p23, t);
    lerp_point(p012, p123, t)
}

fn lerp_point(a: MotionPoint, b: MotionPoint, t: f32) -> MotionPoint {
    MotionPoint {
        time: a.time + (b.time - a.time) * t,
        value: a.value + (b.value - a.value) * t,
    }
}

fn solve_bezier_time(
    start: MotionPoint,
    control1: MotionPoint,
    control2: MotionPoint,
    end: MotionPoint,
    time: f32,
) -> f32 {
    let a = end.time - 3.0 * control2.time + 3.0 * control1.time - start.time;
    let b = 3.0 * control2.time - 6.0 * control1.time + 3.0 * start.time;
    let c = 3.0 * control1.time - 3.0 * start.time;
    let d = start.time - time;
    cardano_algorithm_for_bezier(a, b, c, d)
}

fn cardano_algorithm_for_bezier(a: f32, b: f32, c: f32, d: f32) -> f32 {
    const EPSILON: f32 = 0.00001;
    const CENTER: f32 = 0.5;
    const THRESHOLD: f32 = CENTER + 0.01;

    if a.abs() < EPSILON {
        return quadratic_equation(b, c, d).clamp(0.0, 1.0);
    }

    let ba = b / a;
    let ca = c / a;
    let da = d / a;
    let p = (3.0 * ca - ba * ba) / 3.0;
    let p3 = p / 3.0;
    let q = (2.0 * ba * ba * ba - 9.0 * ba * ca + 27.0 * da) / 27.0;
    let q2 = q / 2.0;
    let discriminant = q2 * q2 + p3 * p3 * p3;

    if discriminant < 0.0 {
        let mp3 = -p / 3.0;
        let mp33 = mp3 * mp3 * mp3;
        let r = mp33.sqrt();
        let t = -q / (2.0 * r);
        let cos_phi = t.clamp(-1.0, 1.0);
        let phi = cos_phi.acos();
        let crtr = r.cbrt();
        let t1 = 2.0 * crtr;

        let root1 = t1 * (phi / 3.0).cos() - ba / 3.0;
        if (root1 - CENTER).abs() < THRESHOLD {
            return root1.clamp(0.0, 1.0);
        }

        let root2 = t1 * ((phi + 2.0 * std::f32::consts::PI) / 3.0).cos() - ba / 3.0;
        if (root2 - CENTER).abs() < THRESHOLD {
            return root2.clamp(0.0, 1.0);
        }

        let root3 = t1 * ((phi + 4.0 * std::f32::consts::PI) / 3.0).cos() - ba / 3.0;
        return root3.clamp(0.0, 1.0);
    }

    if discriminant == 0.0 {
        let u1 = if q2 < 0.0 { (-q2).cbrt() } else { -q2.cbrt() };
        let root1 = 2.0 * u1 - ba / 3.0;
        if (root1 - CENTER).abs() < THRESHOLD {
            return root1.clamp(0.0, 1.0);
        }

        let root2 = -u1 - ba / 3.0;
        return root2.clamp(0.0, 1.0);
    }

    let sd = discriminant.sqrt();
    let u1 = (sd - q2).cbrt();
    let v1 = (sd + q2).cbrt();
    (u1 - v1 - ba / 3.0).clamp(0.0, 1.0)
}

fn quadratic_equation(a: f32, b: f32, c: f32) -> f32 {
    const EPSILON: f32 = 0.00001;

    if a.abs() < EPSILON {
        if b.abs() < EPSILON {
            return -c;
        }
        return -c / b;
    }

    -(b + (b * b - 4.0 * a * c).sqrt()) / (2.0 * a)
}

fn invalid_segments(message: impl Into<String>) -> Error {
    invalid_motion(message)
}

fn invalid_motion(message: impl Into<String>) -> Error {
    Error::InvalidJson {
        format: FORMAT,
        message: message.into(),
    }
}

fn validate_finite(value: f32, name: &str) -> Result<()> {
    if !value.is_finite() {
        return Err(invalid_motion(format!("{name} must be finite")));
    }
    Ok(())
}

fn validate_optional_finite(value: Option<f32>, name: &str) -> Result<()> {
    if let Some(value) = value {
        validate_finite(value, name)?;
    }
    Ok(())
}

fn validate_point(
    point: MotionPoint,
    curve_index: usize,
    segment_index: Option<usize>,
    role: &str,
) -> Result<()> {
    if !point.time.is_finite() {
        return Err(invalid_point(curve_index, segment_index, role, "time"));
    }
    if !point.value.is_finite() {
        return Err(invalid_point(curve_index, segment_index, role, "value"));
    }
    Ok(())
}

fn invalid_point(
    curve_index: usize,
    segment_index: Option<usize>,
    role: &str,
    component: &str,
) -> Error {
    match segment_index {
        Some(segment_index) => invalid_segments(format!(
            "curve {curve_index} segment {segment_index} {role} {component} must be finite"
        )),
        None => invalid_segments(format!(
            "curve {curve_index} {role} {component} must be finite"
        )),
    }
}

fn validate_reported_counts(
    version: u32,
    meta: &MotionMeta,
    actual_curve_count: usize,
    actual_segment_count: usize,
    actual_point_count: usize,
) -> Result<()> {
    for (name, reported, actual) in [
        ("curve count", meta.curve_count, actual_curve_count),
        (
            "total segment count",
            meta.total_segment_count,
            actual_segment_count,
        ),
    ] {
        if u64::from(reported) != actual as u64 {
            return Err(invalid_motion(format!(
                "reported {name} {reported} does not match actual count {actual}"
            )));
        }
    }

    let reported_point_count = u64::from(meta.total_point_count);
    let actual_point_count = actual_point_count as u64;
    // VTube Studio Version 0 recordings report three extra points per curve.
    let vts_version_zero_point_count = actual_point_count + actual_curve_count as u64 * 3;
    if reported_point_count != actual_point_count
        && !(version == 0 && reported_point_count == vts_version_zero_point_count)
    {
        return Err(invalid_motion(format!(
            "reported total point count {} does not match actual count {actual_point_count}",
            meta.total_point_count
        )));
    }
    Ok(())
}
