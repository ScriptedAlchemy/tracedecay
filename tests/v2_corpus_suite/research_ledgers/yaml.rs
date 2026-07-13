use serde_json::{Map, Number, Value};
use std::collections::BTreeMap;

#[derive(Debug)]
struct YamlLine {
    indent: usize,
    text: String,
}

pub(super) fn parse_yaml(source: &str) -> Result<Value, String> {
    let source = expand_yaml_aliases(source)?;
    let lines = source
        .lines()
        .filter_map(|raw| {
            if raw.trim().is_empty() || raw.trim_start().starts_with('#') {
                return None;
            }
            let indent = raw.len() - raw.trim_start_matches(' ').len();
            Some(YamlLine {
                indent,
                text: raw[indent..].trim_end().to_string(),
            })
        })
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return Err("empty YAML document".into());
    }
    let mut index = 0;
    let value = parse_block(&lines, &mut index, lines[0].indent)?;
    if index != lines.len() {
        return Err(format!("unparsed YAML at line index {index}"));
    }
    Ok(value)
}

fn expand_yaml_aliases(source: &str) -> Result<String, String> {
    let mut anchors = BTreeMap::new();
    let mut expanded = Vec::new();
    for line in source.lines() {
        if let Some(marker) = line.find(": &") {
            let anchor = &line[marker + 3..];
            let split = anchor
                .find(char::is_whitespace)
                .ok_or_else(|| format!("anchor has no value: {line}"))?;
            let (name, value) = anchor.split_at(split);
            let value = value.trim_start();
            anchors.insert(name.to_string(), value.to_string());
            expanded.push(format!("{}: {value}", &line[..marker]));
        } else if let Some(marker) = line.find(": *") {
            let name = line[marker + 3..].trim();
            let value = anchors
                .get(name)
                .ok_or_else(|| format!("unknown YAML alias `{name}`"))?;
            expanded.push(format!("{}: {value}", &line[..marker]));
        } else {
            expanded.push(line.to_string());
        }
    }
    Ok(expanded.join("\n"))
}

fn parse_block(lines: &[YamlLine], index: &mut usize, indent: usize) -> Result<Value, String> {
    if lines[*index].indent != indent {
        return Err(format!("unexpected indentation at line index {index}"));
    }
    if lines[*index].text.starts_with("- ") {
        parse_sequence(lines, index, indent)
    } else {
        parse_mapping(lines, index, indent)
    }
}

fn parse_mapping(lines: &[YamlLine], index: &mut usize, indent: usize) -> Result<Value, String> {
    let mut object = Map::new();
    while *index < lines.len()
        && lines[*index].indent == indent
        && !lines[*index].text.starts_with("- ")
    {
        let (key, raw_value) = split_key_value(&lines[*index].text)?;
        *index += 1;
        let value = if raw_value.is_empty() {
            if *index >= lines.len() || lines[*index].indent <= indent {
                return Err(format!("missing nested value for `{key}`"));
            }
            parse_block(lines, index, lines[*index].indent)?
        } else {
            parse_scalar(raw_value)?
        };
        if object.insert(key.to_string(), value).is_some() {
            return Err(format!("duplicate key `{key}`"));
        }
    }
    Ok(Value::Object(object))
}

fn parse_sequence(lines: &[YamlLine], index: &mut usize, indent: usize) -> Result<Value, String> {
    let mut values = Vec::new();
    while *index < lines.len()
        && lines[*index].indent == indent
        && lines[*index].text.starts_with("- ")
    {
        let item = lines[*index].text[2..].trim();
        if let Ok((key, raw_value)) = split_key_value(item) {
            let mut object = Map::new();
            *index += 1;
            let value = if raw_value.is_empty() {
                if *index >= lines.len() || lines[*index].indent <= indent {
                    return Err(format!("missing nested value for `{key}`"));
                }
                parse_block(lines, index, lines[*index].indent)?
            } else {
                parse_scalar(raw_value)?
            };
            object.insert(key.to_string(), value);
            if *index < lines.len() && lines[*index].indent > indent {
                let continuation_indent = lines[*index].indent;
                let continuation = parse_mapping(lines, index, continuation_indent)?;
                for (key, value) in continuation.as_object().expect("mapping") {
                    if object.insert(key.clone(), value.clone()).is_some() {
                        return Err(format!("duplicate sequence mapping key `{key}`"));
                    }
                }
            }
            values.push(Value::Object(object));
        } else {
            values.push(parse_scalar(item)?);
            *index += 1;
        }
    }
    Ok(Value::Array(values))
}

fn split_key_value(value: &str) -> Result<(&str, &str), String> {
    let mut quote = None;
    let mut depth = 0usize;
    for (index, ch) in value.char_indices() {
        match ch {
            '\'' | '"' if quote == Some(ch) => quote = None,
            '\'' | '"' if quote.is_none() => quote = Some(ch),
            '[' | '{' if quote.is_none() => depth += 1,
            ']' | '}' if quote.is_none() => depth = depth.saturating_sub(1),
            ':' if quote.is_none() && depth == 0 => {
                let key = value[..index].trim();
                if key.is_empty() {
                    return Err("empty mapping key".into());
                }
                return Ok((key, value[index + 1..].trim()));
            }
            _ => {}
        }
    }
    Err(format!("expected mapping entry, got `{value}`"))
}

fn parse_scalar(value: &str) -> Result<Value, String> {
    if value.starts_with('[') && value.ends_with(']') {
        return split_inline(&value[1..value.len() - 1], ',')?
            .into_iter()
            .map(parse_scalar)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array);
    }
    if value.starts_with('{') && value.ends_with('}') {
        let mut object = Map::new();
        for item in split_inline(&value[1..value.len() - 1], ',')? {
            let (key, value) = split_key_value(item)?;
            object.insert(key.to_string(), parse_scalar(value)?);
        }
        return Ok(Value::Object(object));
    }
    if value.starts_with('\'') && value.ends_with('\'') {
        return Ok(Value::String(value[1..value.len() - 1].replace("''", "'")));
    }
    if value.starts_with('"') && value.ends_with('"') {
        return serde_json::from_str(value).map_err(|error| error.to_string());
    }
    match value {
        "true" => Ok(Value::Bool(true)),
        "false" => Ok(Value::Bool(false)),
        "null" | "~" => Ok(Value::Null),
        _ => {
            if let Ok(integer) = value.parse::<i64>() {
                Ok(Value::Number(Number::from(integer)))
            } else {
                Ok(Value::String(value.to_string()))
            }
        }
    }
}

fn split_inline(value: &str, separator: char) -> Result<Vec<&str>, String> {
    if value.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    let mut quote = None;
    let mut depth = 0usize;
    let mut start = 0usize;
    for (index, ch) in value.char_indices() {
        match ch {
            '\'' | '"' if quote == Some(ch) => quote = None,
            '\'' | '"' if quote.is_none() => quote = Some(ch),
            '[' | '{' if quote.is_none() => depth += 1,
            ']' | '}' if quote.is_none() => depth = depth.saturating_sub(1),
            _ if ch == separator && quote.is_none() && depth == 0 => {
                result.push(value[start..index].trim());
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    if quote.is_some() || depth != 0 {
        return Err(format!("unterminated inline YAML value `{value}`"));
    }
    result.push(value[start..].trim());
    Ok(result)
}
