//! Self-contained evaluator for CHECK constraint expressions.
//!
//! Parses and evaluates a GQL-style boolean expression against an entity's
//! property map. Supports comparison operators (`=`, `<>`, `<`, `<=`, `>`,
//! `>=`), boolean operators (`AND`, `OR`, `NOT`), `IS NULL`, `IS NOT NULL`,
//! parenthesized sub-expressions, and literal values (integers, floats,
//! strings, booleans, `NULL`).

use std::collections::HashMap;

use grafeo_common::types::Value;

/// Evaluates a CHECK constraint expression against a property map.
///
/// Returns `Ok(true)` when the constraint is satisfied, `Ok(false)` when it
/// is violated, or `Err` if the expression cannot be parsed or evaluated.
pub(crate) fn evaluate_check(
    expression: &str,
    properties: &[(String, Value)],
) -> Result<bool, String> {
    let tokens = tokenize(expression)?;
    let mut pos = 0;
    let ast = parse_or(&tokens, &mut pos)?;
    if pos < tokens.len() {
        return Err(format!(
            "unexpected token after expression: {:?}",
            tokens[pos]
        ));
    }
    let props: HashMap<&str, &Value> = properties.iter().map(|(k, v)| (k.as_str(), v)).collect();
    eval_node(&ast, &props)
}

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Ident(String),
    Integer(i64),
    Float(f64),
    StringLit(String),
    True,
    False,
    Null,
    And,
    Or,
    Not,
    Is,
    In,
    Between,
    LParen,
    RParen,
    Eq,
    Neq,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Comma,
}

fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let ch = chars[i];

        // Skip whitespace
        if ch.is_ascii_whitespace() {
            i += 1;
            continue;
        }

        // Operators and punctuation
        match ch {
            '(' => {
                tokens.push(Token::LParen);
                i += 1;
                continue;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
                continue;
            }
            ',' => {
                tokens.push(Token::Comma);
                i += 1;
                continue;
            }
            '=' => {
                tokens.push(Token::Eq);
                i += 1;
                continue;
            }
            '+' => {
                tokens.push(Token::Plus);
                i += 1;
                continue;
            }
            '*' => {
                tokens.push(Token::Star);
                i += 1;
                continue;
            }
            '/' => {
                tokens.push(Token::Slash);
                i += 1;
                continue;
            }
            '%' => {
                tokens.push(Token::Percent);
                i += 1;
                continue;
            }
            '<' => {
                if i + 1 < len && chars[i + 1] == '>' {
                    tokens.push(Token::Neq);
                    i += 2;
                } else if i + 1 < len && chars[i + 1] == '=' {
                    tokens.push(Token::Le);
                    i += 2;
                } else {
                    tokens.push(Token::Lt);
                    i += 1;
                }
                continue;
            }
            '>' => {
                if i + 1 < len && chars[i + 1] == '=' {
                    tokens.push(Token::Ge);
                    i += 2;
                } else {
                    tokens.push(Token::Gt);
                    i += 1;
                }
                continue;
            }
            '!' => {
                if i + 1 < len && chars[i + 1] == '=' {
                    tokens.push(Token::Neq);
                    i += 2;
                    continue;
                }
                return Err(format!("unexpected character '!' at position {i}"));
            }
            '-' => {
                // Unary minus before a number: only when at start or after an
                // operator / open-paren (i.e., not after a value token).
                let is_unary = tokens.is_empty()
                    || matches!(
                        tokens.last(),
                        Some(
                            Token::LParen
                                | Token::Comma
                                | Token::And
                                | Token::Or
                                | Token::Not
                                | Token::Eq
                                | Token::Neq
                                | Token::Lt
                                | Token::Le
                                | Token::Gt
                                | Token::Ge
                                | Token::Plus
                                | Token::Minus
                                | Token::Star
                                | Token::Slash
                                | Token::Percent
                                | Token::Is
                                | Token::Between
                        )
                    );
                if is_unary && i + 1 < len && (chars[i + 1].is_ascii_digit() || chars[i + 1] == '.')
                {
                    // Consume the number with the minus sign
                    let start = i;
                    i += 1; // skip '-'
                    while i < len
                        && (chars[i].is_ascii_digit()
                            || chars[i] == '.'
                            || chars[i] == 'e'
                            || chars[i] == 'E')
                    {
                        i += 1;
                    }
                    let num_str: String = chars[start..i].iter().collect();
                    if num_str.contains('.') || num_str.contains('e') || num_str.contains('E') {
                        let val: f64 = num_str
                            .parse()
                            .map_err(|e| format!("invalid float '{num_str}': {e}"))?;
                        tokens.push(Token::Float(val));
                    } else {
                        let val: i64 = num_str
                            .parse()
                            .map_err(|e| format!("invalid integer '{num_str}': {e}"))?;
                        tokens.push(Token::Integer(val));
                    }
                } else {
                    tokens.push(Token::Minus);
                    i += 1;
                }
                continue;
            }
            _ => {}
        }

        // String literals (single-quoted)
        if ch == '\'' {
            i += 1;
            let mut s = String::new();
            while i < len {
                if chars[i] == '\'' {
                    if i + 1 < len && chars[i + 1] == '\'' {
                        // Escaped single quote
                        s.push('\'');
                        i += 2;
                    } else {
                        break;
                    }
                } else {
                    s.push(chars[i]);
                    i += 1;
                }
            }
            if i >= len {
                return Err("unterminated string literal".to_string());
            }
            i += 1; // skip closing quote
            tokens.push(Token::StringLit(s));
            continue;
        }

        // Numbers
        if ch.is_ascii_digit() || ch == '.' {
            let start = i;
            while i < len
                && (chars[i].is_ascii_digit()
                    || chars[i] == '.'
                    || chars[i] == 'e'
                    || chars[i] == 'E')
            {
                i += 1;
            }
            let num_str: String = chars[start..i].iter().collect();
            if num_str.contains('.') || num_str.contains('e') || num_str.contains('E') {
                let val: f64 = num_str
                    .parse()
                    .map_err(|e| format!("invalid float '{num_str}': {e}"))?;
                tokens.push(Token::Float(val));
            } else {
                let val: i64 = num_str
                    .parse()
                    .map_err(|e| format!("invalid integer '{num_str}': {e}"))?;
                tokens.push(Token::Integer(val));
            }
            continue;
        }

        // Identifiers and keywords
        if ch.is_ascii_alphabetic() || ch == '_' {
            let start = i;
            while i < len && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let upper = word.to_ascii_uppercase();
            match upper.as_str() {
                "TRUE" => tokens.push(Token::True),
                "FALSE" => tokens.push(Token::False),
                "NULL" => tokens.push(Token::Null),
                "AND" => tokens.push(Token::And),
                "OR" => tokens.push(Token::Or),
                "NOT" => tokens.push(Token::Not),
                "IS" => tokens.push(Token::Is),
                "IN" => tokens.push(Token::In),
                "BETWEEN" => tokens.push(Token::Between),
                _ => tokens.push(Token::Ident(word)),
            }
            continue;
        }

        return Err(format!("unexpected character '{ch}' at position {i}"));
    }

    Ok(tokens)
}

// ---------------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum Expr {
    /// Property reference
    Ident(String),
    /// Literal value
    Literal(Value),
    /// Binary comparison
    Compare {
        left: Box<Expr>,
        op: CmpOp,
        right: Box<Expr>,
    },
    /// Boolean AND
    And(Box<Expr>, Box<Expr>),
    /// Boolean OR
    Or(Box<Expr>, Box<Expr>),
    /// Boolean NOT
    Not(Box<Expr>),
    /// IS NULL
    IsNull(Box<Expr>),
    /// IS NOT NULL
    IsNotNull(Box<Expr>),
    /// Arithmetic operation
    Arithmetic {
        left: Box<Expr>,
        op: ArithOp,
        right: Box<Expr>,
    },
    /// value IN (list)
    InList {
        value: Box<Expr>,
        list: Vec<Expr>,
        negated: bool,
    },
    /// value BETWEEN low AND high
    Between {
        value: Box<Expr>,
        low: Box<Expr>,
        high: Box<Expr>,
        negated: bool,
    },
}

#[derive(Debug, Clone, Copy)]
enum CmpOp {
    Eq,
    Neq,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, Copy)]
enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

// ---------------------------------------------------------------------------
// Recursive-descent parser
//
// Grammar:
//   expr        -> or_expr
//   or_expr     -> and_expr (OR and_expr)*
//   and_expr    -> not_expr (AND not_expr)*
//   not_expr    -> NOT not_expr | comparison
//   comparison  -> addition ((= | <> | < | <= | > | >=) addition)?
//                | addition IS [NOT] NULL
//                | addition [NOT] IN (list)
//                | addition [NOT] BETWEEN addition AND addition
//   addition    -> multiply ((+ | -) multiply)*
//   multiply    -> unary ((* | / | %) unary)*
//   unary       -> - unary | primary
//   primary     -> Ident | Number | StringLit | True | False | Null
//                | ( expr )
// ---------------------------------------------------------------------------

fn parse_or(tokens: &[Token], pos: &mut usize) -> Result<Expr, String> {
    let mut left = parse_and(tokens, pos)?;
    while *pos < tokens.len() && tokens[*pos] == Token::Or {
        *pos += 1;
        let right = parse_and(tokens, pos)?;
        left = Expr::Or(Box::new(left), Box::new(right));
    }
    Ok(left)
}

fn parse_and(tokens: &[Token], pos: &mut usize) -> Result<Expr, String> {
    let mut left = parse_not(tokens, pos)?;
    while *pos < tokens.len() && tokens[*pos] == Token::And {
        *pos += 1;
        let right = parse_not(tokens, pos)?;
        left = Expr::And(Box::new(left), Box::new(right));
    }
    Ok(left)
}

fn parse_not(tokens: &[Token], pos: &mut usize) -> Result<Expr, String> {
    if *pos < tokens.len() && tokens[*pos] == Token::Not {
        *pos += 1;
        let inner = parse_not(tokens, pos)?;
        return Ok(Expr::Not(Box::new(inner)));
    }
    parse_comparison(tokens, pos)
}

fn parse_comparison(tokens: &[Token], pos: &mut usize) -> Result<Expr, String> {
    let left = parse_addition(tokens, pos)?;

    if *pos < tokens.len() {
        // IS [NOT] NULL
        if tokens[*pos] == Token::Is {
            *pos += 1;
            if *pos < tokens.len() && tokens[*pos] == Token::Not {
                *pos += 1;
                expect_token(tokens, pos, &Token::Null, "NULL")?;
                return Ok(Expr::IsNotNull(Box::new(left)));
            }
            expect_token(tokens, pos, &Token::Null, "NULL")?;
            return Ok(Expr::IsNull(Box::new(left)));
        }

        // [NOT] IN (list)
        if tokens[*pos] == Token::In {
            *pos += 1;
            let list = parse_in_list(tokens, pos)?;
            return Ok(Expr::InList {
                value: Box::new(left),
                list,
                negated: false,
            });
        }
        if tokens[*pos] == Token::Not && *pos + 1 < tokens.len() && tokens[*pos + 1] == Token::In {
            *pos += 2;
            let list = parse_in_list(tokens, pos)?;
            return Ok(Expr::InList {
                value: Box::new(left),
                list,
                negated: true,
            });
        }

        // [NOT] BETWEEN low AND high
        if tokens[*pos] == Token::Between {
            *pos += 1;
            return parse_between_rest(left, false, tokens, pos);
        }
        if tokens[*pos] == Token::Not
            && *pos + 1 < tokens.len()
            && tokens[*pos + 1] == Token::Between
        {
            *pos += 2;
            return parse_between_rest(left, true, tokens, pos);
        }

        // Comparison operators
        let op = match tokens[*pos] {
            Token::Eq => Some(CmpOp::Eq),
            Token::Neq => Some(CmpOp::Neq),
            Token::Lt => Some(CmpOp::Lt),
            Token::Le => Some(CmpOp::Le),
            Token::Gt => Some(CmpOp::Gt),
            Token::Ge => Some(CmpOp::Ge),
            _ => None,
        };
        if let Some(op) = op {
            *pos += 1;
            let right = parse_addition(tokens, pos)?;
            return Ok(Expr::Compare {
                left: Box::new(left),
                op,
                right: Box::new(right),
            });
        }
    }

    Ok(left)
}

fn parse_in_list(tokens: &[Token], pos: &mut usize) -> Result<Vec<Expr>, String> {
    expect_token(tokens, pos, &Token::LParen, "(")?;
    let mut items = Vec::new();
    if *pos < tokens.len() && tokens[*pos] != Token::RParen {
        items.push(parse_addition(tokens, pos)?);
        while *pos < tokens.len() && tokens[*pos] == Token::Comma {
            *pos += 1;
            items.push(parse_addition(tokens, pos)?);
        }
    }
    expect_token(tokens, pos, &Token::RParen, ")")?;
    Ok(items)
}

fn parse_between_rest(
    value: Expr,
    negated: bool,
    tokens: &[Token],
    pos: &mut usize,
) -> Result<Expr, String> {
    let low = parse_addition(tokens, pos)?;
    expect_token(tokens, pos, &Token::And, "AND")?;
    let high = parse_addition(tokens, pos)?;
    Ok(Expr::Between {
        value: Box::new(value),
        low: Box::new(low),
        high: Box::new(high),
        negated,
    })
}

fn parse_addition(tokens: &[Token], pos: &mut usize) -> Result<Expr, String> {
    let mut left = parse_multiply(tokens, pos)?;
    while *pos < tokens.len() {
        let op = match tokens[*pos] {
            Token::Plus => ArithOp::Add,
            Token::Minus => ArithOp::Sub,
            _ => break,
        };
        *pos += 1;
        let right = parse_multiply(tokens, pos)?;
        left = Expr::Arithmetic {
            left: Box::new(left),
            op,
            right: Box::new(right),
        };
    }
    Ok(left)
}

fn parse_multiply(tokens: &[Token], pos: &mut usize) -> Result<Expr, String> {
    let mut left = parse_unary(tokens, pos)?;
    while *pos < tokens.len() {
        let op = match tokens[*pos] {
            Token::Star => ArithOp::Mul,
            Token::Slash => ArithOp::Div,
            Token::Percent => ArithOp::Mod,
            _ => break,
        };
        *pos += 1;
        let right = parse_unary(tokens, pos)?;
        left = Expr::Arithmetic {
            left: Box::new(left),
            op,
            right: Box::new(right),
        };
    }
    Ok(left)
}

fn parse_unary(tokens: &[Token], pos: &mut usize) -> Result<Expr, String> {
    if *pos < tokens.len() && tokens[*pos] == Token::Minus {
        *pos += 1;
        let inner = parse_unary(tokens, pos)?;
        return Ok(Expr::Arithmetic {
            left: Box::new(Expr::Literal(Value::Int64(0))),
            op: ArithOp::Sub,
            right: Box::new(inner),
        });
    }
    parse_primary(tokens, pos)
}

fn parse_primary(tokens: &[Token], pos: &mut usize) -> Result<Expr, String> {
    if *pos >= tokens.len() {
        return Err("unexpected end of expression".to_string());
    }
    match &tokens[*pos] {
        Token::Ident(name) => {
            let name = name.clone();
            *pos += 1;
            Ok(Expr::Ident(name))
        }
        Token::Integer(n) => {
            let n = *n;
            *pos += 1;
            Ok(Expr::Literal(Value::Int64(n)))
        }
        Token::Float(f) => {
            let f = *f;
            *pos += 1;
            Ok(Expr::Literal(Value::Float64(f)))
        }
        Token::StringLit(s) => {
            let s = s.clone();
            *pos += 1;
            Ok(Expr::Literal(Value::String(s.into())))
        }
        Token::True => {
            *pos += 1;
            Ok(Expr::Literal(Value::Bool(true)))
        }
        Token::False => {
            *pos += 1;
            Ok(Expr::Literal(Value::Bool(false)))
        }
        Token::Null => {
            *pos += 1;
            Ok(Expr::Literal(Value::Null))
        }
        Token::LParen => {
            *pos += 1;
            let inner = parse_or(tokens, pos)?;
            expect_token(tokens, pos, &Token::RParen, ")")?;
            Ok(inner)
        }
        other => Err(format!("unexpected token: {other:?}")),
    }
}

fn expect_token(
    tokens: &[Token],
    pos: &mut usize,
    expected: &Token,
    label: &str,
) -> Result<(), String> {
    if *pos >= tokens.len() {
        return Err(format!("expected {label}, found end of expression"));
    }
    if &tokens[*pos] != expected {
        return Err(format!("expected {label}, found {:?}", tokens[*pos]));
    }
    *pos += 1;
    Ok(())
}

// ---------------------------------------------------------------------------
// Evaluator
// ---------------------------------------------------------------------------

fn eval_node(expr: &Expr, props: &HashMap<&str, &Value>) -> Result<bool, String> {
    match expr {
        Expr::Literal(Value::Bool(b)) => Ok(*b),
        Expr::Literal(Value::Null) => Ok(false),
        Expr::Literal(_) => Err("non-boolean literal in boolean context".to_string()),
        Expr::Ident(name) => {
            let val = props.get(name.as_str()).copied().unwrap_or(&Value::Null);
            match val {
                Value::Bool(b) => Ok(*b),
                Value::Null => Ok(false),
                _ => Err(format!(
                    "property '{name}' is not boolean, cannot use directly as a condition"
                )),
            }
        }
        Expr::And(left, right) => Ok(eval_node(left, props)? && eval_node(right, props)?),
        Expr::Or(left, right) => Ok(eval_node(left, props)? || eval_node(right, props)?),
        Expr::Not(inner) => Ok(!eval_node(inner, props)?),
        Expr::IsNull(inner) => {
            let val = eval_value(inner, props)?;
            Ok(val == Value::Null)
        }
        Expr::IsNotNull(inner) => {
            let val = eval_value(inner, props)?;
            Ok(val != Value::Null)
        }
        Expr::Compare { left, op, right } => {
            let lval = eval_value(left, props)?;
            let rval = eval_value(right, props)?;
            // NULL comparisons always yield false (SQL/GQL three-valued logic)
            if lval == Value::Null || rval == Value::Null {
                return Ok(false);
            }
            eval_compare(&lval, *op, &rval)
        }
        Expr::Arithmetic { .. } => {
            // An arithmetic expression in boolean context: check if it is truthy.
            // This is not standard GQL, so error out.
            Err("arithmetic expression in boolean context".to_string())
        }
        Expr::InList {
            value,
            list,
            negated,
        } => {
            let val = eval_value(value, props)?;
            if val == Value::Null {
                return Ok(false);
            }
            let mut found = false;
            for item in list {
                let item_val = eval_value(item, props)?;
                if item_val != Value::Null && val == item_val {
                    found = true;
                    break;
                }
            }
            Ok(if *negated { !found } else { found })
        }
        Expr::Between {
            value,
            low,
            high,
            negated,
        } => {
            let val = eval_value(value, props)?;
            let lo = eval_value(low, props)?;
            let hi = eval_value(high, props)?;
            if val == Value::Null || lo == Value::Null || hi == Value::Null {
                return Ok(false);
            }
            let ge_low = eval_compare(&val, CmpOp::Ge, &lo)?;
            let le_high = eval_compare(&val, CmpOp::Le, &hi)?;
            let in_range = ge_low && le_high;
            Ok(if *negated { !in_range } else { in_range })
        }
    }
}

/// Evaluates an expression to a `Value` (not necessarily boolean).
fn eval_value(expr: &Expr, props: &HashMap<&str, &Value>) -> Result<Value, String> {
    match expr {
        Expr::Literal(v) => Ok(v.clone()),
        Expr::Ident(name) => {
            let val = props.get(name.as_str()).copied().unwrap_or(&Value::Null);
            Ok(val.clone())
        }
        Expr::Arithmetic {
            left, op, right, ..
        } => {
            let lval = eval_value(left, props)?;
            let rval = eval_value(right, props)?;
            if lval == Value::Null || rval == Value::Null {
                return Ok(Value::Null);
            }
            eval_arithmetic(&lval, *op, &rval)
        }
        // Boolean expressions evaluated as a value produce Bool
        Expr::Compare { .. }
        | Expr::And(_, _)
        | Expr::Or(_, _)
        | Expr::Not(_)
        | Expr::IsNull(_)
        | Expr::IsNotNull(_)
        | Expr::InList { .. }
        | Expr::Between { .. } => {
            let b = eval_node(expr, props)?;
            Ok(Value::Bool(b))
        }
    }
}

fn eval_compare(left: &Value, op: CmpOp, right: &Value) -> Result<bool, String> {
    match op {
        CmpOp::Eq => Ok(left == right),
        CmpOp::Neq => Ok(left != right),
        _ => {
            let ordering = compare_values(left, right)
                .ok_or_else(|| format!("cannot compare {left:?} with {right:?}"))?;
            Ok(match op {
                CmpOp::Lt => ordering == std::cmp::Ordering::Less,
                CmpOp::Le => {
                    ordering == std::cmp::Ordering::Less || ordering == std::cmp::Ordering::Equal
                }
                CmpOp::Gt => ordering == std::cmp::Ordering::Greater,
                CmpOp::Ge => {
                    ordering == std::cmp::Ordering::Greater || ordering == std::cmp::Ordering::Equal
                }
                CmpOp::Eq | CmpOp::Neq => unreachable!(),
            })
        }
    }
}

/// Orders two values for relational comparison.
fn compare_values(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::Int64(a), Value::Int64(b)) => Some(a.cmp(b)),
        (Value::Float64(a), Value::Float64(b)) => a.partial_cmp(b),
        (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
        (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
        // Cross-type numeric promotion
        (Value::Int64(a), Value::Float64(b)) => (*a as f64).partial_cmp(b),
        (Value::Float64(a), Value::Int64(b)) => a.partial_cmp(&(*b as f64)),
        _ => None,
    }
}

fn eval_arithmetic(left: &Value, op: ArithOp, right: &Value) -> Result<Value, String> {
    match (left, right) {
        (Value::Int64(a), Value::Int64(b)) => {
            let result = match op {
                ArithOp::Add => a.checked_add(*b).ok_or("integer overflow")?,
                ArithOp::Sub => a.checked_sub(*b).ok_or("integer underflow")?,
                ArithOp::Mul => a.checked_mul(*b).ok_or("integer overflow")?,
                ArithOp::Div => {
                    if *b == 0 {
                        return Err("division by zero".to_string());
                    }
                    a.checked_div(*b).ok_or("integer overflow")?
                }
                ArithOp::Mod => {
                    if *b == 0 {
                        return Err("modulo by zero".to_string());
                    }
                    a.checked_rem(*b).ok_or("integer overflow")?
                }
            };
            Ok(Value::Int64(result))
        }
        (Value::Float64(a), Value::Float64(b)) => {
            let result = match op {
                ArithOp::Add => a + b,
                ArithOp::Sub => a - b,
                ArithOp::Mul => a * b,
                ArithOp::Div => a / b,
                ArithOp::Mod => a % b,
            };
            Ok(Value::Float64(result))
        }
        // Cross-type promotion: Int64 + Float64 -> Float64
        (Value::Int64(a), Value::Float64(b)) => {
            eval_arithmetic(&Value::Float64(*a as f64), op, &Value::Float64(*b))
        }
        (Value::Float64(a), Value::Int64(b)) => {
            eval_arithmetic(&Value::Float64(*a), op, &Value::Float64(*b as f64))
        }
        _ => Err(format!(
            "unsupported arithmetic between {left:?} and {right:?}"
        )),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn props(pairs: &[(&str, Value)]) -> Vec<(String, Value)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    // -- Basic comparisons --

    #[test]
    fn test_integer_equality() {
        let p = props(&[("age", Value::Int64(30))]);
        assert!(evaluate_check("age = 30", &p).unwrap());
        assert!(!evaluate_check("age = 25", &p).unwrap());
    }

    #[test]
    fn test_integer_inequality() {
        let p = props(&[("age", Value::Int64(30))]);
        assert!(evaluate_check("age <> 25", &p).unwrap());
        assert!(!evaluate_check("age <> 30", &p).unwrap());
    }

    #[test]
    fn test_integer_ordering() {
        let p = props(&[("age", Value::Int64(30))]);
        assert!(evaluate_check("age > 18", &p).unwrap());
        assert!(evaluate_check("age >= 30", &p).unwrap());
        assert!(evaluate_check("age < 100", &p).unwrap());
        assert!(evaluate_check("age <= 30", &p).unwrap());
        assert!(!evaluate_check("age < 30", &p).unwrap());
        assert!(!evaluate_check("age > 30", &p).unwrap());
    }

    #[test]
    fn test_float_comparison() {
        let p = props(&[("score", Value::Float64(3.15))]);
        assert!(evaluate_check("score > 3.0", &p).unwrap());
        assert!(evaluate_check("score < 4.0", &p).unwrap());
    }

    #[test]
    fn test_string_comparison() {
        let p = props(&[("name", Value::String("Gus".into()))]);
        assert!(evaluate_check("name = 'Gus'", &p).unwrap());
        assert!(!evaluate_check("name = 'Alix'", &p).unwrap());
    }

    #[test]
    fn test_cross_type_numeric() {
        let p = props(&[("score", Value::Int64(10))]);
        assert!(evaluate_check("score > 9.5", &p).unwrap());
        assert!(!evaluate_check("score > 10.5", &p).unwrap());
    }

    // -- Boolean operators --

    #[test]
    fn test_and() {
        let p = props(&[("age", Value::Int64(25)), ("score", Value::Int64(90))]);
        assert!(evaluate_check("age >= 18 AND score >= 80", &p).unwrap());
        assert!(!evaluate_check("age >= 30 AND score >= 80", &p).unwrap());
    }

    #[test]
    fn test_or() {
        let p = props(&[("age", Value::Int64(15))]);
        assert!(evaluate_check("age < 18 OR age > 65", &p).unwrap());
        assert!(!evaluate_check("age > 18 OR age < 10", &p).unwrap());
    }

    #[test]
    fn test_not() {
        let p = props(&[("active", Value::Bool(false))]);
        assert!(evaluate_check("NOT active", &p).unwrap());
        assert!(!evaluate_check("active", &p).unwrap());
    }

    #[test]
    fn test_combined_boolean() {
        let p = props(&[("age", Value::Int64(25)), ("vip", Value::Bool(true))]);
        assert!(evaluate_check("age >= 18 AND (vip OR age > 30)", &p).unwrap());
    }

    // -- NULL handling --

    #[test]
    fn test_is_null() {
        let p = props(&[("email", Value::Null)]);
        assert!(evaluate_check("email IS NULL", &p).unwrap());
        assert!(!evaluate_check("email IS NOT NULL", &p).unwrap());
    }

    #[test]
    fn test_is_not_null() {
        let p = props(&[("email", Value::String("a@b.com".into()))]);
        assert!(evaluate_check("email IS NOT NULL", &p).unwrap());
        assert!(!evaluate_check("email IS NULL", &p).unwrap());
    }

    #[test]
    fn test_missing_property_is_null() {
        let p = props(&[]);
        assert!(evaluate_check("phantom IS NULL", &p).unwrap());
    }

    #[test]
    fn test_null_comparison_is_false() {
        // SQL/GQL: NULL = NULL -> false, NULL <> NULL -> false
        let p = props(&[("x", Value::Null)]);
        assert!(!evaluate_check("x = 1", &p).unwrap());
        assert!(!evaluate_check("x <> 1", &p).unwrap());
        assert!(!evaluate_check("x > 0", &p).unwrap());
    }

    // -- Arithmetic in comparisons --

    #[test]
    fn test_arithmetic_addition() {
        let p = props(&[("price", Value::Int64(100)), ("tax", Value::Int64(20))]);
        assert!(evaluate_check("price + tax = 120", &p).unwrap());
    }

    #[test]
    fn test_arithmetic_subtraction() {
        let p = props(&[("a", Value::Int64(50))]);
        assert!(evaluate_check("a - 10 > 30", &p).unwrap());
    }

    #[test]
    fn test_arithmetic_multiplication() {
        let p = props(&[("qty", Value::Int64(5)), ("price", Value::Int64(10))]);
        assert!(evaluate_check("qty * price = 50", &p).unwrap());
    }

    #[test]
    fn test_arithmetic_modulo() {
        let p = props(&[("x", Value::Int64(10))]);
        assert!(evaluate_check("x % 3 = 1", &p).unwrap());
    }

    // -- IN list --

    #[test]
    fn test_in_list() {
        let p = props(&[("status", Value::String("active".into()))]);
        assert!(evaluate_check("status IN ('active', 'pending')", &p).unwrap());
        assert!(!evaluate_check("status IN ('closed', 'archived')", &p).unwrap());
    }

    #[test]
    fn test_not_in_list() {
        let p = props(&[("status", Value::String("active".into()))]);
        assert!(evaluate_check("status NOT IN ('closed', 'archived')", &p).unwrap());
        assert!(!evaluate_check("status NOT IN ('active', 'pending')", &p).unwrap());
    }

    // -- BETWEEN --

    #[test]
    fn test_between() {
        let p = props(&[("age", Value::Int64(25))]);
        assert!(evaluate_check("age BETWEEN 18 AND 65", &p).unwrap());
        assert!(!evaluate_check("age BETWEEN 30 AND 65", &p).unwrap());
    }

    #[test]
    fn test_not_between() {
        let p = props(&[("age", Value::Int64(10))]);
        assert!(evaluate_check("age NOT BETWEEN 18 AND 65", &p).unwrap());
        assert!(!evaluate_check("age NOT BETWEEN 5 AND 15", &p).unwrap());
    }

    // -- Edge cases --

    #[test]
    fn test_escaped_string() {
        let p = props(&[("name", Value::String("it's".into()))]);
        assert!(evaluate_check("name = 'it''s'", &p).unwrap());
    }

    #[test]
    fn test_nested_parentheses() {
        let p = props(&[("x", Value::Int64(5))]);
        assert!(evaluate_check("((x > 1) AND (x < 10))", &p).unwrap());
    }

    #[test]
    fn test_negative_number() {
        let p = props(&[("temp", Value::Int64(-5))]);
        assert!(evaluate_check("temp < 0", &p).unwrap());
        assert!(evaluate_check("temp = -5", &p).unwrap());
    }

    #[test]
    fn test_bool_literal_true() {
        let p = props(&[("active", Value::Bool(true))]);
        assert!(evaluate_check("active = TRUE", &p).unwrap());
    }

    #[test]
    fn test_bang_equals() {
        let p = props(&[("x", Value::Int64(5))]);
        assert!(evaluate_check("x != 3", &p).unwrap());
        assert!(!evaluate_check("x != 5", &p).unwrap());
    }

    // -- Error cases --

    #[test]
    fn test_empty_expression_error() {
        let p = props(&[]);
        assert!(evaluate_check("", &p).is_err());
    }

    #[test]
    fn test_unterminated_string_error() {
        let p = props(&[]);
        assert!(evaluate_check("name = 'oops", &p).is_err());
    }

    #[test]
    fn test_division_by_zero_error() {
        let p = props(&[("x", Value::Int64(10))]);
        assert!(evaluate_check("x / 0 = 1", &p).is_err());
    }

    #[test]
    fn test_incomparable_types_error() {
        let p = props(&[("x", Value::Bool(true))]);
        assert!(evaluate_check("x > 5", &p).is_err());
    }

    // -- Arithmetic in boolean context --

    #[test]
    fn test_arithmetic_in_boolean_context_errors() {
        let p = props(&[("x", Value::Int64(5))]);
        assert!(evaluate_check("x + 1", &p).is_err());
    }

    // -- Integer overflow / underflow --

    #[test]
    fn test_integer_overflow_add() {
        let p = props(&[("x", Value::Int64(i64::MAX))]);
        assert!(evaluate_check("x + 1 > 0", &p).is_err());
    }

    #[test]
    fn test_integer_underflow_sub() {
        let p = props(&[("x", Value::Int64(i64::MIN))]);
        assert!(evaluate_check("x - 1 < 0", &p).is_err());
    }

    #[test]
    fn test_integer_overflow_mul() {
        let p = props(&[("x", Value::Int64(i64::MAX))]);
        assert!(evaluate_check("x * 2 > 0", &p).is_err());
    }

    #[test]
    fn test_modulo_by_zero() {
        let p = props(&[("x", Value::Int64(10))]);
        assert!(evaluate_check("x % 0 = 0", &p).is_err());
    }

    // -- Float arithmetic --

    #[test]
    fn test_float_arithmetic() {
        let p = props(&[("x", Value::Float64(2.5))]);
        assert!(evaluate_check("x * 2.0 = 5.0", &p).unwrap());
        assert!(evaluate_check("x + 1.5 = 4.0", &p).unwrap());
        assert!(evaluate_check("x - 0.5 = 2.0", &p).unwrap());
    }

    #[test]
    fn test_float_division() {
        let p = props(&[("x", Value::Float64(10.0))]);
        assert!(evaluate_check("x / 2.0 = 5.0", &p).unwrap());
    }

    #[test]
    fn test_float_modulo() {
        let p = props(&[("x", Value::Float64(10.0))]);
        assert!(evaluate_check("x % 3.0 = 1.0", &p).unwrap());
    }

    // -- Cross-type numeric promotion --

    #[test]
    fn test_int_float_cross_promotion() {
        let p = props(&[("x", Value::Int64(5)), ("y", Value::Float64(2.5))]);
        assert!(evaluate_check("x + y = 7.5", &p).unwrap());
        assert!(evaluate_check("y * x = 12.5", &p).unwrap());
    }

    // -- Unsupported arithmetic types --

    #[test]
    fn test_arithmetic_on_strings_errors() {
        let p = props(&[("x", Value::String("hello".into()))]);
        assert!(evaluate_check("x + 1 = 2", &p).is_err());
    }

    // -- Negated IN and BETWEEN --

    #[test]
    fn test_not_in_list_generic() {
        let p = props(&[("x", Value::Int64(5))]);
        assert!(evaluate_check("x NOT IN (1, 2, 3)", &p).unwrap());
        assert!(!evaluate_check("x NOT IN (5, 6, 7)", &p).unwrap());
    }

    #[test]
    fn test_not_between_generic() {
        let p = props(&[("x", Value::Int64(5))]);
        assert!(evaluate_check("x NOT BETWEEN 10 AND 20", &p).unwrap());
        assert!(!evaluate_check("x NOT BETWEEN 1 AND 10", &p).unwrap());
    }

    // -- Null propagation in arithmetic --

    #[test]
    fn test_null_in_arithmetic_comparison() {
        let p = props(&[("x", Value::Null)]);
        // NULL + 1 comparison should yield false (not error)
        assert!(!evaluate_check("x > 5", &p).unwrap());
    }

    // -- Complex nested boolean --

    #[test]
    fn test_nested_and_or_not() {
        let p = props(&[("x", Value::Int64(5)), ("y", Value::Int64(10))]);
        assert!(evaluate_check("(x > 0 AND y > 0) OR x < -100", &p).unwrap());
        assert!(evaluate_check("NOT (x > 100)", &p).unwrap());
        assert!(!evaluate_check("NOT (x > 0 AND y > 0)", &p).unwrap());
    }
}
