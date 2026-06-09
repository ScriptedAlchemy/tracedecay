const LARGE_TOOL_OUTPUT_CHARS: usize = 256 * 1024;
const LONG_BASE64_RUN_CHARS: usize = 64 * 1024;
const BINARYISH_SAMPLE_CHARS: usize = 8192;

pub fn should_externalize(role: &str, kind: Option<&str>, content: &str) -> bool {
    if content.trim_start().starts_with("data:") {
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
    for byte in content.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=') {
            run += 1;
            if run >= LONG_BASE64_RUN_CHARS {
                return true;
            }
        } else {
            run = 0;
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
