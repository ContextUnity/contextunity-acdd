//! Boolean `when:` expression parser, normalizer, and evaluator for task blocks and gates.

use acdd_core::errors::{ErrorCode, StateError};
use std::collections::BTreeSet;
use std::fmt;

/// Parsed boolean predicate expression over task tags (`tag`, `!tag`, `&`/`+`, `|`, parentheses).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum WhenExpr {
    /// Positive tag match (`tag`).
    Tag(String),
    /// Logical negation (`!expr`).
    Not(Box<WhenExpr>),
    /// Logical conjunction (`left & right` or `left + right`).
    And(Box<WhenExpr>, Box<WhenExpr>),
    /// Logical disjunction (`left | right`).
    Or(Box<WhenExpr>, Box<WhenExpr>),
}

impl WhenExpr {
    /// Parse a boolean `when:` expression string.
    pub fn parse(raw: &str) -> Result<Self, StateError> {
        let tokens = tokenize(raw)?;
        if tokens.is_empty() {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                "when expression cannot be empty",
            ));
        }
        let mut parser = Parser {
            tokens: &tokens,
            pos: 0,
        };
        let expr = parser.parse_or()?;
        if parser.pos < parser.tokens.len() {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("unexpected trailing token in when expression '{raw}'"),
            ));
        }
        Ok(expr)
    }

    /// Evaluate this predicate against the task's effective tag set.
    pub fn eval(&self, tags: &[String]) -> bool {
        match self {
            Self::Tag(tag) => tags.iter().any(|item| item == tag),
            Self::Not(inner) => !inner.eval(tags),
            Self::And(left, right) => left.eval(tags) && right.eval(tags),
            Self::Or(left, right) => left.eval(tags) || right.eval(tags),
        }
    }

    /// Collect every tag identifier referenced in this expression.
    pub fn referenced_tags(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        self.collect_tags(&mut out);
        out
    }

    fn collect_tags(&self, out: &mut BTreeSet<String>) {
        match self {
            Self::Tag(tag) => {
                out.insert(tag.clone());
            }
            Self::Not(inner) => inner.collect_tags(out),
            Self::And(left, right) | Self::Or(left, right) => {
                left.collect_tags(out);
                right.collect_tags(out);
            }
        }
    }

    /// Validate that every referenced tag is `"default"` or declared in `allowed_tags`.
    pub fn validate_tags(
        &self,
        allowed_tags: &BTreeSet<String>,
        context: &str,
    ) -> Result<(), StateError> {
        for tag in self.referenced_tags() {
            if tag != "default" && !allowed_tags.contains(&tag) {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("{context}: unknown tag '{tag}' in when expression"),
                ));
            }
        }
        Ok(())
    }

    /// Return a canonical normalized string representation for structural `- tasks:` matching.
    pub fn normalized(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for WhenExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tag(tag) => f.write_str(tag),
            Self::Not(inner) => match inner.as_ref() {
                Self::Tag(_) | Self::Not(_) => write!(f, "!{inner}"),
                Self::And(_, _) | Self::Or(_, _) => write!(f, "!({inner})"),
            },
            Self::And(left, right) => {
                write_and_operand(left, f)?;
                f.write_str(" & ")?;
                write_and_operand(right, f)
            }
            Self::Or(left, right) => write!(f, "{left} | {right}"),
        }
    }
}

fn write_and_operand(expr: &WhenExpr, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match expr {
        WhenExpr::Or(_, _) => write!(f, "({expr})"),
        _ => write!(f, "{expr}"),
    }
}

/// Normalize an optional `when:` expression string for structural patch comparison.
pub fn normalize_optional_when(raw: Option<&str>) -> Result<Option<String>, StateError> {
    match raw {
        None => Ok(None),
        Some(text) => Ok(Some(WhenExpr::parse(text)?.normalized())),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Ident(String),
    Bang,
    And,
    Or,
    LParen,
    RParen,
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

fn is_ident_cont(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn tokenize(raw: &str) -> Result<Vec<Token>, StateError> {
    let bytes = raw.as_bytes();
    let mut i = 0usize;
    let mut tokens = Vec::new();
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match b {
            b'!' => {
                tokens.push(Token::Bang);
                i += 1;
            }
            b'+' => {
                tokens.push(Token::And);
                i += 1;
            }
            b'&' => {
                if bytes.get(i + 1) == Some(&b'&') {
                    i += 2;
                } else {
                    i += 1;
                }
                tokens.push(Token::And);
            }
            b'|' => {
                if bytes.get(i + 1) == Some(&b'|') {
                    i += 2;
                } else {
                    i += 1;
                }
                tokens.push(Token::Or);
            }
            b'(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            b')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            _ if is_ident_start(b) => {
                let start = i;
                i += 1;
                while i < bytes.len() && is_ident_cont(bytes[i]) {
                    i += 1;
                }
                let ident = &raw[start..i];
                if ident.len() > 64 {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("tag '{ident}' in when expression exceeds 64 characters"),
                    ));
                }
                tokens.push(Token::Ident(ident.to_owned()));
            }
            _ => {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!(
                        "invalid character '{}' in when expression '{raw}'",
                        raw[i..].chars().next().unwrap_or('?')
                    ),
                ));
            }
        }
    }
    Ok(tokens)
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let tok = self.tokens.get(self.pos).cloned();
        if tok.is_some() {
            self.pos += 1;
        }
        tok
    }

    fn parse_or(&mut self) -> Result<WhenExpr, StateError> {
        let mut left = self.parse_and()?;
        while matches!(self.peek(), Some(Token::Or)) {
            self.next();
            let right = self.parse_and()?;
            left = WhenExpr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<WhenExpr, StateError> {
        let mut left = self.parse_not()?;
        while matches!(self.peek(), Some(Token::And)) {
            self.next();
            let right = self.parse_not()?;
            left = WhenExpr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<WhenExpr, StateError> {
        if matches!(self.peek(), Some(Token::Bang)) {
            self.next();
            let inner = self.parse_not()?;
            return Ok(WhenExpr::Not(Box::new(inner)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<WhenExpr, StateError> {
        match self.next() {
            Some(Token::Ident(name)) => Ok(WhenExpr::Tag(name)),
            Some(Token::LParen) => {
                let expr = self.parse_or()?;
                match self.next() {
                    Some(Token::RParen) => Ok(expr),
                    _ => Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        "unclosed '(' in when expression",
                    )),
                }
            }
            Some(tok) => Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("unexpected token {tok:?} in when expression"),
            )),
            None => Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                "unexpected end of when expression",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn when_expression_parser_and_eval_matrix() {
        let allowed = BTreeSet::from([
            "code".to_owned(),
            "docs".to_owned(),
            "db_migration".to_owned(),
        ]);

        let eval_cases: &[(&str, &[&str], bool, &str)] = &[
            ("code", &["code"], true, "code"),
            ("code", &["docs"], false, "code"),
            ("code", &["default"], false, "code"),
            ("default", &["default"], true, "default"),
            ("!docs", &["code"], true, "!docs"),
            ("!docs", &["docs"], false, "!docs"),
            ("code + !docs", &["code"], true, "code & !docs"),
            ("code & !docs", &["code", "docs"], false, "code & !docs"),
            ("code + !docs", &["code", "docs"], false, "code & !docs"),
            (
                "(code | docs) & !db_migration",
                &["docs"],
                true,
                "(code | docs) & !db_migration",
            ),
            (
                "(code | docs) && !db_migration",
                &["docs", "db_migration"],
                false,
                "(code | docs) & !db_migration",
            ),
            ("default || code", &["default"], true, "default | code"),
        ];

        for &(raw, tags, expect_eval, expect_norm) in eval_cases {
            let expr = WhenExpr::parse(raw).expect("valid when");
            expr.validate_tags(&allowed, "test").expect("known tags");
            let tag_vec: Vec<String> = tags.iter().map(|s| (*s).to_owned()).collect();
            assert_eq!(expr.eval(&tag_vec), expect_eval, "eval {raw} on {tags:?}");
            assert_eq!(expr.normalized(), expect_norm, "norm {raw}");
        }

        let bad_cases: &[(&str, bool)] = &[
            ("", false),
            ("(code", false),
            ("code)", false),
            ("& code", false),
            ("code &", false),
            ("unknown_tag", true),
        ];

        for &(raw, parses_ok) in bad_cases {
            let parsed = WhenExpr::parse(raw);
            if parses_ok {
                let expr = parsed.expect("should parse");
                let err = expr
                    .validate_tags(&allowed, "test")
                    .expect_err("unknown tag");
                assert_eq!(err.code, ErrorCode::ProfileSchemaInvalid);
            } else {
                let err = parsed.expect_err("should fail parse");
                assert_eq!(err.code, ErrorCode::ProfileSchemaInvalid);
            }
        }
    }
}
