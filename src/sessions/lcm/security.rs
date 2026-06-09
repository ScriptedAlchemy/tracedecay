const LARGE_TOOL_OUTPUT_CHARS: usize = 256 * 1024;
const LONG_BASE64_RUN_CHARS: usize = 64 * 1024;
const BINARYISH_SAMPLE_CHARS: usize = 8192;

pub fn should_externalize(role: &str, kind: Option<&str>, content: &str) -> bool {
    if contains_data_uri(content) {
        return true;
    }
    if has_long_base64_run(content) {
        return true;
    }
    if is_binaryish(content) {
        return true;
    }
    is_tool_payload(role, kind) && content.chars().count() > LARGE_TOOL_OUTPUT_CHARS
}

fn contains_data_uri(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    for (idx, _) in lower.match_indices("data:") {
        let after = &lower[idx + "data:".len()..];
        let mut saw_comma = false;
        for ch in after.chars().take(256) {
            if ch == ',' {
                saw_comma = true;
                break;
            }
            if ch.is_whitespace() || matches!(ch, '"' | '\'' | '<' | '>') {
                break;
            }
        }
        if saw_comma {
            return true;
        }
    }
    false
}

fn is_tool_payload(role: &str, kind: Option<&str>) -> bool {
    role.eq_ignore_ascii_case("tool")
        || kind
            .map(|value| {
                let value = value.to_ascii_lowercase();
                value == "tool_result" || value == "tool_output"
            })
            .unwrap_or(false)
}

fn has_long_base64_run(content: &str) -> bool {
    let mut run = 0usize;
    let mut distinct = [false; 256];
    let mut distinct_count = 0usize;
    let mut has_symbol = false;
    let mut has_upper = false;
    let mut has_lower = false;
    let mut has_digit = false;
    for byte in content.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=') {
            run += 1;
            if !distinct[byte as usize] {
                distinct[byte as usize] = true;
                distinct_count += 1;
            }
            has_symbol |= matches!(byte, b'+' | b'/' | b'=');
            has_upper |= byte.is_ascii_uppercase();
            has_lower |= byte.is_ascii_lowercase();
            has_digit |= byte.is_ascii_digit();
            if run >= LONG_BASE64_RUN_CHARS {
                let categories =
                    usize::from(has_upper) + usize::from(has_lower) + usize::from(has_digit);
                return has_symbol || (categories >= 2 && distinct_count >= 8);
            }
        } else {
            run = 0;
            distinct = [false; 256];
            distinct_count = 0;
            has_symbol = false;
            has_upper = false;
            has_lower = false;
            has_digit = false;
        }
    }
    false
}

fn is_binaryish(content: &str) -> bool {
    let sample = content.chars().take(BINARYISH_SAMPLE_CHARS);
    let mut total = 0usize;
    let mut control = 0usize;
    for ch in sample {
        total += 1;
        if ch == '\0' || (ch.is_control() && !matches!(ch, '\n' | '\r' | '\t')) {
            control += 1;
        }
    }
    total >= 1024 && control * 10 > total
}
