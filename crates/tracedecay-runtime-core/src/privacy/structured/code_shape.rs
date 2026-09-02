use super::{is_assignment_key, is_yaml_preamble_line, looks_like_yaml_mapping};

pub(super) fn has_code_shape_context(text: &str) -> bool {
    for line in text.lines().map(str::trim_start) {
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with("//")
            || is_yaml_preamble_line(line)
        {
            continue;
        }
        if looks_like_code_line(line) {
            return true;
        }
        if looks_like_yaml_mapping(line)
            || line
                .split_once('=')
                .is_some_and(|(key, _)| is_assignment_key(key.trim()))
        {
            return false;
        }
    }
    false
}

fn looks_like_code_line(line: &str) -> bool {
    let declaration = line
        .strip_prefix("pub ")
        .or_else(|| {
            line.strip_prefix("pub(")
                .and_then(|rest| rest.split_once(')'))
                .map(|(_, rest)| rest.trim_start())
        })
        .unwrap_or(line);
    let declaration = declaration
        .strip_prefix("async ")
        .or_else(|| declaration.strip_prefix("unsafe "))
        .unwrap_or(declaration);

    ((declaration.starts_with("fn ") || declaration.starts_with("function "))
        && declaration.contains('('))
        || ((line.starts_with("let ") || line.starts_with("const ")) && line.contains('='))
        || ((declaration.starts_with("class ")
            || declaration.starts_with("impl ")
            || declaration.starts_with("struct ")
            || declaration.starts_with("enum "))
            && declaration.contains('{'))
        || ((line.starts_with("if ")
            || line.starts_with("for ")
            || line.starts_with("while ")
            || line.starts_with("match "))
            && line.contains(['{', '(']))
        || (line.ends_with('{') && !line.contains([':', '=']))
        || (line
            .find("=>")
            .is_some_and(|arrow| !line[..arrow].contains('='))
            && !looks_like_yaml_mapping(line))
        || (line.ends_with(';')
            && line
                .split_once('=')
                .is_some_and(|(key, _)| key.ends_with(char::is_whitespace)))
        || (line.starts_with("return ") && line.ends_with(';'))
        || ((line.starts_with("import ") || line.starts_with("export ")) && line.ends_with(';'))
}
