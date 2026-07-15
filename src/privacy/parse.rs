use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParseFailureKind {
    Empty,
    TooLarge,
    Malformed,
    NonObject,
    TooDeep,
    TooManyValues,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ParseLimits {
    pub max_record_bytes: usize,
    pub max_depth: usize,
    pub max_values: usize,
}

pub(crate) fn parse_claude_record(
    record: &[u8],
    limits: ParseLimits,
) -> Result<Value, ParseFailureKind> {
    if record.is_empty() {
        return Err(ParseFailureKind::Empty);
    }
    if record.len() > limits.max_record_bytes {
        return Err(ParseFailureKind::TooLarge);
    }

    let value = serde_json::from_slice::<Value>(record).map_err(|_| ParseFailureKind::Malformed)?;
    if !value.is_object() {
        return Err(ParseFailureKind::NonObject);
    }
    validate_structure(&value, limits)?;
    Ok(value)
}

fn validate_structure(value: &Value, limits: ParseLimits) -> Result<(), ParseFailureKind> {
    let mut stack = vec![(value, 1usize)];
    let mut visited = 0usize;
    while let Some((current, depth)) = stack.pop() {
        visited = visited.saturating_add(1);
        if visited > limits.max_values {
            return Err(ParseFailureKind::TooManyValues);
        }
        if depth > limits.max_depth {
            return Err(ParseFailureKind::TooDeep);
        }
        match current {
            Value::Object(fields) => {
                stack.extend(
                    fields
                        .values()
                        .map(|child| (child, depth.saturating_add(1))),
                );
            }
            Value::Array(items) => {
                stack.extend(items.iter().map(|child| (child, depth.saturating_add(1))));
            }
            _ => {}
        }
    }
    Ok(())
}
