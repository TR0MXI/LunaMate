use crate::platform::underlay::exact_buffer_scale;

#[test]
fn integer_buffer_scale_requires_exact_matching_axes() {
    assert_eq!(exact_buffer_scale(800, 400), Some(2));
    assert_eq!(exact_buffer_scale(500, 400), None);
    assert_eq!(exact_buffer_scale(0, 0), None);
}
