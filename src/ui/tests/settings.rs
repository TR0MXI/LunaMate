use crate::{
    config::FrameRate,
    ui::settings::{custom_frame_rate_seed, parse_custom_frame_rate},
};

#[test]
fn custom_frame_rate_input_accepts_only_positive_u16_digits() {
    assert!(matches!(
        parse_custom_frame_rate("60"),
        Some(FrameRate::Custom(fps)) if fps.get() == 60
    ));
    assert!(matches!(
        parse_custom_frame_rate("65535"),
        Some(FrameRate::Custom(fps)) if fps.get() == u16::MAX
    ));
    for invalid in ["", "0", "-1", "+60", "60.0", "65536", "６０"] {
        assert_eq!(parse_custom_frame_rate(invalid), None);
    }
}

#[test]
fn custom_frame_rate_seed_uses_fixed_rate_or_sixty() {
    assert_eq!(custom_frame_rate_seed(FrameRate::Fps30), 30);
    assert_eq!(custom_frame_rate_seed(FrameRate::Fps120), 120);
    assert_eq!(custom_frame_rate_seed(FrameRate::FollowDisplay), 60);
    assert_eq!(custom_frame_rate_seed(FrameRate::Unlimited), 60);
    assert_eq!(
        custom_frame_rate_seed(FrameRate::custom(75).expect("测试帧率必须有效")),
        75
    );
}
