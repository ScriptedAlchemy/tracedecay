//! JSON surface mounted by the source-provenance build script.
//!
//! `build-support/source_provenance.rs` names the `serde_json` crate. This
//! member supplies that crate name from the fixture workspace, so the cargo
//! rerun tests do not download or vendor a registry closure.

use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

#[derive(Debug)]
pub struct Error {
    message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => formatter.write_str("null"),
            Value::Bool(true) => formatter.write_str("true"),
            Value::Bool(false) => formatter.write_str("false"),
            Value::Number(raw) => formatter.write_str(raw),
            Value::String(text) => write_json_string(formatter, text),
            Value::Array(items) => {
                formatter.write_str("[")?;
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str(",")?;
                    }
                    write!(formatter, "{item}")?;
                }
                formatter.write_str("]")
            }
            Value::Object(entries) => {
                formatter.write_str("{")?;
                for (index, (key, value)) in entries.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str(",")?;
                    }
                    write_json_string(formatter, key)?;
                    write!(formatter, ":{value}")?;
                }
                formatter.write_str("}")
            }
        }
    }
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        let Value::Object(entries) = self else {
            return None;
        };
        entries
            .iter()
            .rev()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(value) => Some(*value),
            _ => None,
        }
    }
}

pub fn from_slice(bytes: &[u8]) -> Result<Value, Error> {
    let text = std::str::from_utf8(bytes).map_err(|_| error("invalid utf-8"))?;
    let mut parser = Parser {
        input: text,
        index: 0,
    };
    let value = parser.parse_value()?;
    parser.skip_ws();
    if parser.index != parser.input.len() {
        return Err(error("trailing data"));
    }
    Ok(value)
}

fn write_json_string(formatter: &mut fmt::Formatter<'_>, text: &str) -> fmt::Result {
    formatter.write_str("\"")?;
    for character in text.chars() {
        match character {
            '"' => formatter.write_str("\\\"")?,
            '\\' => formatter.write_str("\\\\")?,
            '\n' => formatter.write_str("\\n")?,
            '\r' => formatter.write_str("\\r")?,
            '\t' => formatter.write_str("\\t")?,
            character if (character as u32) < 0x20 => {
                write!(formatter, "\\u{:04x}", character as u32)?;
            }
            character => write!(formatter, "{character}")?,
        }
    }
    formatter.write_str("\"")
}

fn error(message: impl Into<String>) -> Error {
    Error {
        message: message.into(),
    }
}

struct Parser<'a> {
    input: &'a str,
    index: usize,
}

impl<'a> Parser<'a> {
    fn parse_value(&mut self) -> Result<Value, Error> {
        self.skip_ws();
        match self.peek() {
            Some('{') => self.parse_object(),
            Some('[') => self.parse_array(),
            Some('"') => self.parse_string().map(Value::String),
            Some('t') => self.parse_literal("true", Value::Bool(true)),
            Some('f') => self.parse_literal("false", Value::Bool(false)),
            Some('n') => self.parse_literal("null", Value::Null),
            Some(character) if character == '-' || character.is_ascii_digit() => {
                self.parse_number()
            }
            Some(character) => Err(error(format!("unexpected {character}"))),
            None => Err(error("unexpected end")),
        }
    }

    fn parse_object(&mut self) -> Result<Value, Error> {
        self.expect('{')?;
        let mut entries = Vec::new();
        loop {
            self.skip_ws();
            if self.consume('}') {
                return Ok(Value::Object(entries));
            }
            if !entries.is_empty() {
                self.expect(',')?;
                self.skip_ws();
            }
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(':')?;
            let value = self.parse_value()?;
            if let Some(existing) = entries.iter_mut().find(|(name, _)| name == &key) {
                existing.1 = value;
            } else {
                entries.push((key, value));
            }
        }
    }

    fn parse_array(&mut self) -> Result<Value, Error> {
        self.expect('[')?;
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            if self.consume(']') {
                return Ok(Value::Array(items));
            }
            if !items.is_empty() {
                self.expect(',')?;
            }
            items.push(self.parse_value()?);
        }
    }

    fn parse_string(&mut self) -> Result<String, Error> {
        self.expect('"')?;
        let mut text = String::new();
        loop {
            match self.bump()? {
                '"' => return Ok(text),
                '\\' => text.push(self.parse_escape()?),
                character if (character as u32) < 0x20 => {
                    return Err(error("unescaped control character"));
                }
                character => text.push(character),
            }
        }
    }

    fn parse_escape(&mut self) -> Result<char, Error> {
        match self.bump()? {
            '"' => Ok('"'),
            '\\' => Ok('\\'),
            '/' => Ok('/'),
            'b' => Ok('\u{0008}'),
            'f' => Ok('\u{000c}'),
            'n' => Ok('\n'),
            'r' => Ok('\r'),
            't' => Ok('\t'),
            'u' => self.parse_unicode_escape(),
            character => Err(error(format!("invalid escape \\{character}"))),
        }
    }

    fn parse_unicode_escape(&mut self) -> Result<char, Error> {
        let mut unit = 0u32;
        for _ in 0..4 {
            let digit = self.bump()?;
            let value = digit
                .to_digit(16)
                .ok_or_else(|| error("invalid unicode escape"))?;
            unit = (unit << 4) | value;
        }
        char::from_u32(unit).ok_or_else(|| error("invalid unicode escape"))
    }

    fn parse_number(&mut self) -> Result<Value, Error> {
        let start = self.index;
        if self.peek() == Some('-') {
            self.index += 1;
        }
        match self.peek() {
            Some('0') => self.index += 1,
            Some(character) if character.is_ascii_digit() => {
                while self
                    .peek()
                    .is_some_and(|character| character.is_ascii_digit())
                {
                    self.index += 1;
                }
            }
            _ => return Err(error("invalid number")),
        }
        if self.peek() == Some('.') {
            self.index += 1;
            if !self.consume_digits() {
                return Err(error("invalid number"));
            }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            self.index += 1;
            if matches!(self.peek(), Some('+' | '-')) {
                self.index += 1;
            }
            if !self.consume_digits() {
                return Err(error("invalid number"));
            }
        }
        Ok(Value::Number(self.input[start..self.index].to_string()))
    }

    fn parse_literal(&mut self, literal: &str, value: Value) -> Result<Value, Error> {
        if self.input[self.index..].starts_with(literal) {
            self.index += literal.len();
            Ok(value)
        } else {
            Err(error(format!("expected {literal}")))
        }
    }

    fn consume_digits(&mut self) -> bool {
        let start = self.index;
        while self
            .peek()
            .is_some_and(|character| character.is_ascii_digit())
        {
            self.index += 1;
        }
        self.index > start
    }

    fn expect(&mut self, expected: char) -> Result<(), Error> {
        if self.consume(expected) {
            Ok(())
        } else {
            Err(error(format!("expected {expected}")))
        }
    }

    fn consume(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.index += expected.len_utf8();
            true
        } else {
            false
        }
    }

    fn bump(&mut self) -> Result<char, Error> {
        let character = self.peek().ok_or_else(|| error("unexpected end"))?;
        self.index += character.len_utf8();
        Ok(character)
    }

    fn peek(&self) -> Option<char> {
        self.input[self.index..].chars().next()
    }

    fn skip_ws(&mut self) {
        while self
            .peek()
            .is_some_and(|character| character.is_ascii_whitespace())
        {
            self.index += 1;
        }
    }
}
