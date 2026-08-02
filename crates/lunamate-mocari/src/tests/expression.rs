use std::sync::Arc;

use crate::{expression::ExpressionPlayer, json::Expression3};

#[test]
fn players_share_parsed_expression_but_keep_independent_transition_state() {
    let expression = Arc::new(
        Expression3::from_json_str(
            r#"{"Type":"Live2D Expression","Parameters":[{"Id":"ParamAngleX","Value":1,"Blend":"Overwrite"}]}"#,
        )
        .expect("固定共享表情应可解析"),
    );
    let mut first = ExpressionPlayer::new(Arc::clone(&expression));
    let second = ExpressionPlayer::new(Arc::clone(&expression));

    assert_eq!(
        first.parsed_identity_for_test(),
        second.parsed_identity_for_test(),
        "播放器必须共享不可变解析参数"
    );
    first.tick(0.5);
    first.start_fade_out();
    first.set_weight(0.25);
    assert_eq!(first.time(), 0.5);
    assert_eq!(second.time(), 0.0);
    assert!(first.is_fading_out());
    assert!(!second.is_fading_out());
    assert_eq!(first.weight(), 0.25);
    assert_eq!(second.weight(), 1.0);
}
