use std::{fs, ops::Range, path::Path};

struct MacroCall<'a> {
    offset: usize,
    body: &'a str,
}

#[test]
fn production_log_macros_use_stable_language_independent_events() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    collect_production_sources(&workspace.join("src"), &mut files);
    collect_production_sources(
        &workspace.join("crates").join("lunamate-agent").join("src"),
        &mut files,
    );
    files.sort();

    let mut call_count = 0;
    for path in files {
        let source = fs::read_to_string(&path).expect("生产 Rust 源码应当可读取为 UTF-8");
        for call in log_macro_calls(&source) {
            call_count += 1;
            let line = source[..call.offset]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
                + 1;
            let location = format!("{}:{line}", path.display());
            let literals = string_literals(call.body);
            let event_literal = literals
                .iter()
                .find(|literal| event_name(literal).is_some())
                .unwrap_or_else(|| panic!("{location} 的日志宏缺少字面 event="));
            let event = event_name(event_literal).expect("上一步已经确认 event 存在");

            assert!(
                event
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'),
                "{location} 的 event 不是 snake_case：{event}"
            );
            assert!(
                literals.iter().all(|literal| !contains_cjk(literal)),
                "{location} 的日志宏包含 app-owned CJK 字面消息"
            );
            assert!(
                literals
                    .iter()
                    .all(|literal| !literal.trim_start().starts_with("log.")),
                "{location} 的日志宏不得调用 log.* 本地化 key"
            );
            assert!(
                literals.iter().all(|literal| !literal.contains("message=")),
                "{location} 的日志宏不得携带自然语言 message 字段"
            );
            assert!(
                structured_format_literal(event_literal),
                "{location} 的 event 格式串包含非结构化 prose：{event_literal}"
            );
            assert!(
                literals.iter().all(|literal| {
                    event_name(literal).is_some()
                        || !literal.bytes().any(|byte| byte.is_ascii_whitespace())
                }),
                "{location} 的日志宏包含额外自然语言字面参数"
            );
        }
    }

    assert!(
        call_count >= 150,
        "扫描范围异常，生产日志宏仅 {call_count} 处"
    );
}

#[test]
fn policy_scanner_ignores_comments_and_variable_values() {
    let source = r#"
        fn fixture(detail: &str) {
            log::warn!(
                // 中文注释不属于持久化消息。
                "event=policy_fixture detail={}",
                detail
            );
        }
    "#;

    let calls = log_macro_calls(source);
    assert_eq!(calls.len(), 1);
    let literals = string_literals(calls[0].body);
    assert_eq!(literals, ["event=policy_fixture detail={}"]);
    assert_eq!(event_name(literals[0]), Some("policy_fixture"));
    assert!(literals.iter().all(|literal| !contains_cjk(literal)));
}

fn collect_production_sources(directory: &Path, output: &mut Vec<std::path::PathBuf>) {
    let entries = fs::read_dir(directory).expect("生产源码目录应当可枚举");
    for entry in entries {
        let entry = entry.expect("生产源码目录项应当可读取");
        let path = entry.path();
        let file_type = entry.file_type().expect("生产源码目录项类型应当可读取");
        if file_type.is_dir() {
            if entry.file_name() != "tests" {
                collect_production_sources(&path, output);
            }
        } else if file_type.is_file() && path.extension().is_some_and(|extension| extension == "rs")
        {
            output.push(path);
        }
    }
}

fn log_macro_calls(source: &str) -> Vec<MacroCall<'_>> {
    let bytes = source.as_bytes();
    let mut calls = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if let Some(end) = skipped_syntax_end(source, index) {
            index = end;
            continue;
        }
        if !is_identifier_start(bytes[index]) {
            index += 1;
            continue;
        }

        let start = index;
        let mut segments = Vec::new();
        loop {
            let segment_start = index;
            index += 1;
            while index < bytes.len() && is_identifier_continue(bytes[index]) {
                index += 1;
            }
            segments.push(&source[segment_start..index]);
            let separator = skip_ascii_whitespace(bytes, index);
            if bytes.get(separator..separator + 2) != Some(b"::") {
                index = separator;
                break;
            }
            let next = skip_ascii_whitespace(bytes, separator + 2);
            if next >= bytes.len() || !is_identifier_start(bytes[next]) {
                index = next;
                break;
            }
            index = next;
        }

        let is_log_macro = matches!(segments.as_slice(), [level] if is_log_level(level))
            || matches!(segments.as_slice(), [namespace, level] if *namespace == "log" && is_log_level(level));
        let bang = skip_ascii_whitespace(bytes, index);
        if !is_log_macro || bytes.get(bang) != Some(&b'!') {
            index = index.max(start + 1);
            continue;
        }
        let open = skip_ascii_whitespace(bytes, bang + 1);
        if !bytes.get(open).is_some_and(|byte| is_open_delimiter(*byte)) {
            index = open.max(start + 1);
            continue;
        }
        let close = matching_delimiter(source, open)
            .unwrap_or_else(|| panic!("日志宏从字节 {start} 起没有闭合定界符"));
        calls.push(MacroCall {
            offset: start,
            body: &source[open + 1..close],
        });
        index = close + 1;
    }
    calls
}

fn string_literals(source: &str) -> Vec<&str> {
    let bytes = source.as_bytes();
    let mut literals = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if let Some((content, end)) = raw_string(source, index) {
            literals.push(&source[content]);
            index = end;
            continue;
        }
        if bytes[index] == b'"' {
            let (content, end) = quoted_string(source, index);
            literals.push(&source[content]);
            index = end;
            continue;
        }
        if let Some(end) = comment_end(source, index).or_else(|| character_end(source, index)) {
            index = end;
            continue;
        }
        index += 1;
    }
    literals
}

fn event_name(literal: &str) -> Option<&str> {
    let start = literal.find("event=")? + "event=".len();
    let event = &literal[start..];
    let end = event
        .bytes()
        .position(|byte| !(byte.is_ascii_alphanumeric() || byte == b'_'))
        .unwrap_or(event.len());
    (end > 0).then_some(&event[..end])
}

fn structured_format_literal(literal: &str) -> bool {
    literal.split_ascii_whitespace().all(|field| {
        let Some((name, _)) = field.split_once('=') else {
            return false;
        };
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    })
}

fn contains_cjk(value: &str) -> bool {
    value.chars().any(|character| {
        matches!(
            character,
            '\u{2e80}'..='\u{9fff}'
                | '\u{f900}'..='\u{faff}'
                | '\u{20000}'..='\u{2fa1f}'
                | '\u{ac00}'..='\u{d7af}'
        )
    })
}

fn matching_delimiter(source: &str, open: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut stack = vec![bytes[open]];
    let mut index = open + 1;
    while index < bytes.len() {
        if let Some(end) = skipped_syntax_end(source, index) {
            index = end;
            continue;
        }
        if is_open_delimiter(bytes[index]) {
            stack.push(bytes[index]);
        } else if is_close_delimiter(bytes[index]) {
            let expected = matching_close(*stack.last()?);
            if bytes[index] == expected {
                stack.pop();
                if stack.is_empty() {
                    return Some(index);
                }
            }
        }
        index += 1;
    }
    None
}

fn skipped_syntax_end(source: &str, index: usize) -> Option<usize> {
    raw_string(source, index)
        .map(|(_, end)| end)
        .or_else(|| {
            (source.as_bytes().get(index) == Some(&b'"')).then(|| quoted_string(source, index).1)
        })
        .or_else(|| comment_end(source, index))
        .or_else(|| character_end(source, index))
}

fn raw_string(source: &str, index: usize) -> Option<(Range<usize>, usize)> {
    let bytes = source.as_bytes();
    let raw = if bytes.get(index) == Some(&b'r') {
        index
    } else if matches!(bytes.get(index), Some(b'b' | b'c')) && bytes.get(index + 1) == Some(&b'r') {
        index + 1
    } else {
        return None;
    };
    let mut quote = raw + 1;
    while bytes.get(quote) == Some(&b'#') {
        quote += 1;
    }
    if bytes.get(quote) != Some(&b'"') {
        return None;
    }
    let hashes = quote - raw - 1;
    let content_start = quote + 1;
    let mut cursor = content_start;
    while cursor < bytes.len() {
        if bytes[cursor] == b'"'
            && bytes.get(cursor + 1..cursor + 1 + hashes) == Some(&bytes[raw + 1..quote])
        {
            return Some((content_start..cursor, cursor + hashes + 1));
        }
        cursor += 1;
    }
    Some((content_start..bytes.len(), bytes.len()))
}

fn quoted_string(source: &str, quote: usize) -> (Range<usize>, usize) {
    let bytes = source.as_bytes();
    let mut index = quote + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index = (index + 2).min(bytes.len()),
            b'"' => return (quote + 1..index, index + 1),
            _ => index += 1,
        }
    }
    (quote + 1..bytes.len(), bytes.len())
}

fn comment_end(source: &str, index: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    match bytes.get(index..index + 2) {
        Some(b"//") => Some(
            bytes[index + 2..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |offset| index + offset + 3),
        ),
        Some(b"/*") => {
            let mut depth = 1_u32;
            let mut cursor = index + 2;
            while cursor < bytes.len() && depth > 0 {
                match bytes.get(cursor..cursor + 2) {
                    Some(b"/*") => {
                        depth += 1;
                        cursor += 2;
                    }
                    Some(b"*/") => {
                        depth -= 1;
                        cursor += 2;
                    }
                    _ => cursor += 1,
                }
            }
            Some(cursor)
        }
        _ => None,
    }
}

fn character_end(source: &str, quote: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    if bytes.get(quote) != Some(&b'\'') {
        return None;
    }
    let limit = (quote + 18).min(bytes.len());
    let mut index = quote + 1;
    while index < limit {
        match bytes[index] {
            b'\\' => index = (index + 2).min(limit),
            b'\'' => return Some(index + 1),
            _ => index += 1,
        }
    }
    None
}

fn skip_ascii_whitespace(bytes: &[u8], mut index: usize) -> usize {
    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
        index += 1;
    }
    index
}

fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_identifier_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn is_log_level(value: &str) -> bool {
    matches!(value, "trace" | "debug" | "info" | "warn" | "error")
}

fn is_open_delimiter(byte: u8) -> bool {
    matches!(byte, b'(' | b'[' | b'{')
}

fn is_close_delimiter(byte: u8) -> bool {
    matches!(byte, b')' | b']' | b'}')
}

fn matching_close(open: u8) -> u8 {
    match open {
        b'(' => b')',
        b'[' => b']',
        b'{' => b'}',
        _ => unreachable!("调用方只传入已校验的开放定界符"),
    }
}
