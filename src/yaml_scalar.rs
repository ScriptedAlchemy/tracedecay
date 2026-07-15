use std::{borrow::Cow, error::Error, fmt};

#[derive(Debug)]
pub(crate) enum YamlScalarError {
    MalformedSingleQuoted,
    MalformedDoubleQuoted(serde_json::Error),
}

impl fmt::Display for YamlScalarError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedSingleQuoted => {
                formatter.write_str("malformed single-quoted YAML scalar")
            }
            Self::MalformedDoubleQuoted(error) => {
                write!(formatter, "malformed double-quoted YAML scalar: {error}")
            }
        }
    }
}

impl Error for YamlScalarError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::MalformedSingleQuoted => None,
            Self::MalformedDoubleQuoted(error) => Some(error),
        }
    }
}

pub(crate) fn decode_yaml_scalar(value: &str) -> Result<Cow<'_, str>, YamlScalarError> {
    if let Some(quoted) = value.strip_prefix('\'') {
        let Some(inner) = quoted.strip_suffix('\'') else {
            return Err(YamlScalarError::MalformedSingleQuoted);
        };

        let mut chars = inner.chars().peekable();
        let mut has_doubled_quote = false;
        while let Some(ch) = chars.next() {
            if ch == '\'' {
                if chars.next() != Some('\'') {
                    return Err(YamlScalarError::MalformedSingleQuoted);
                }
                has_doubled_quote = true;
            }
        }

        return Ok(if has_doubled_quote {
            Cow::Owned(inner.replace("''", "'"))
        } else {
            Cow::Borrowed(inner)
        });
    }

    if value.starts_with('"') {
        return serde_json::from_str::<String>(value)
            .map(Cow::Owned)
            .map_err(YamlScalarError::MalformedDoubleQuoted);
    }

    Ok(Cow::Borrowed(value))
}

#[cfg(test)]
mod tests {
    use super::{YamlScalarError, decode_yaml_scalar};
    use std::borrow::Cow;

    #[test]
    fn borrows_plain_scalars_without_applying_policy() {
        assert!(matches!(
            decode_yaml_scalar("plain value"),
            Ok(Cow::Borrowed("plain value"))
        ));
        assert!(matches!(decode_yaml_scalar(""), Ok(Cow::Borrowed(""))));
        assert!(matches!(
            decode_yaml_scalar(" users'"),
            Ok(Cow::Borrowed(" users'"))
        ));
    }

    #[test]
    fn decodes_single_quoted_scalars_and_doubled_quotes() {
        assert!(matches!(
            decode_yaml_scalar("'plain'"),
            Ok(Cow::Borrowed("plain"))
        ));
        let decoded = decode_yaml_scalar("'it''s YAML'").expect("valid single-quoted scalar");
        assert_eq!(decoded, "it's YAML");
    }

    #[test]
    fn decodes_json_compatible_double_quoted_scalars() {
        let decoded = decode_yaml_scalar(r#""line\n☺""#).expect("valid double-quoted scalar");
        assert_eq!(decoded, "line\n☺");
    }

    #[test]
    fn rejects_malformed_single_quoted_scalars() {
        for value in ["'", "'unterminated", "'isn't valid'", "'triple'''quote'"] {
            assert!(matches!(
                decode_yaml_scalar(value),
                Err(YamlScalarError::MalformedSingleQuoted)
            ));
        }
    }

    #[test]
    fn rejects_malformed_double_quoted_scalars() {
        for value in [
            r#""unterminated"#,
            r#""bad\xescape""#,
            r#""value" trailing"#,
        ] {
            assert!(matches!(
                decode_yaml_scalar(value),
                Err(YamlScalarError::MalformedDoubleQuoted(_))
            ));
        }
    }
}
