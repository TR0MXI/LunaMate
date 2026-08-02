use crate::logging::{max_log_field_bytes_for_test, sanitize_log_field};

#[test]
fn private_log_fields_are_bounded_and_single_line() {
    let sanitized = sanitize_log_field(format!(
        "safe value\r\n\x1b[31m\u{7}中\\tail{}",
        "x".repeat(max_log_field_bytes_for_test() * 2)
    ));

    assert!(sanitized.len() <= max_log_field_bytes_for_test());
    assert!(sanitized.is_char_boundary(sanitized.len()));
    assert!(sanitized.contains("safe\\x20value\\r\\n\\u{1b}[31m\\u{7}中\\\\tail"));
    assert!(
        sanitized
            .chars()
            .all(|character| !matches!(character, '\r' | '\n' | '\x1b' | '\u{7}'))
    );
}
