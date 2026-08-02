//! 提供 Agent 私有日志边界使用的有界字段净化。

use std::{fmt, fmt::Write as _};

const MAX_LOG_FIELD_BYTES: usize = 512;

struct SanitizedLogField {
    output: String,
}

impl SanitizedLogField {
    fn new() -> Self {
        Self {
            output: String::with_capacity(MAX_LOG_FIELD_BYTES),
        }
    }

    fn push(&mut self, value: &str) -> fmt::Result {
        if self.output.len().saturating_add(value.len()) > MAX_LOG_FIELD_BYTES {
            return Err(fmt::Error);
        }
        self.output.push_str(value);
        Ok(())
    }
}

impl fmt::Write for SanitizedLogField {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        for character in value.chars() {
            match character {
                '\\' => self.push("\\\\")?,
                ' ' => self.push("\\x20")?,
                '\r' => self.push("\\r")?,
                '\n' => self.push("\\n")?,
                '\t' => self.push("\\t")?,
                character if character.is_control() => {
                    self.push(&format!("\\u{{{:x}}}", u32::from(character)))?;
                }
                character => {
                    let mut encoded = [0; 4];
                    self.push(character.encode_utf8(&mut encoded))?;
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn sanitize_log_field(value: impl fmt::Display) -> String {
    let mut sanitized = SanitizedLogField::new();
    let _ = write!(&mut sanitized, "{value}");
    sanitized.output
}

#[cfg(test)]
pub(crate) const fn max_log_field_bytes_for_test() -> usize {
    MAX_LOG_FIELD_BYTES
}
