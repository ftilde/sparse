use std::{iter::Peekable, str::CharIndices};

use matrix_sdk::ruma::events::AnySyncMessageEvent;

use crate::timeline::Event;
use regex::Regex;

#[derive(Clone)]
pub enum Filter {
    Sender(Regex),
    Body(Regex),
    Not(Box<Filter>),
    And(Vec<Filter>),
    Or(Vec<Filter>),
}
impl Filter {
    pub fn matches(&self, event: &Event) -> bool {
        match self {
            Filter::Sender(sender) => sender.is_match(event.sender().as_str()),
            Filter::Body(body) => {
                if let Event::Message(AnySyncMessageEvent::RoomMessage(m)) = event {
                    body.is_match(crate::tui_app::tui::messages::strip_body(m.content.body()))
                } else {
                    false
                }
            }
            Filter::Not(v) => !v.matches(event),
            Filter::And(v) => v.iter().all(|f| f.matches(event)),
            Filter::Or(v) => v.iter().any(|f| f.matches(event)),
        }
    }
    pub fn parse(s: &str) -> Result<Self, String> {
        let tokens = tokenize(s).collect::<Result<Vec<_>, TokenizeError>>();
        let tokens = match tokens {
            Ok(t) => t,
            Err(TokenizeError::InvalidEscape(b, e)) => {
                let s = if let Some(e) = e { &s[b..e] } else { &s[b..] };
                return Err(format!("Invalid escape expression: {}", s));
            }
            Err(TokenizeError::UnfinishedString(r)) => {
                return Err(format!("Unfinished string: {}", &s[r]))
            }
            Err(TokenizeError::UnfinishedType(r)) => {
                return Err(format!("Unfinished filter type: {}", &s[r]))
            }
        };
        let tokens = tokens;
        let f = parse_from_tokens(&mut &tokens[..])?;
        let f = f.build()?;
        Ok(f)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FilterExpression {
    Sender(String),
    Body(String),
    Not(Box<FilterExpression>),
    And(Vec<FilterExpression>),
    Or(Vec<FilterExpression>),
}

#[derive(Debug, PartialEq)]
enum Token {
    LParen,
    RParen,
    Bar,
    Exclamation,
    Type(char),
    Text(String),
    Whitespace(String),
}

fn tok_string(chars: &mut Peekable<CharIndices<'_>>) -> Result<Token, TokenizeError> {
    let mut out = String::new();
    let begin = chars.next().unwrap();
    assert_eq!(begin.1, '"');
    loop {
        if let Some((i, c)) = chars.peek().cloned() {
            match c {
                '"' => {
                    let _ = chars.next();
                    return Ok(Token::Text(out));
                }
                '\\' => {
                    let _ = chars.next();
                    if let Some((_, c)) = chars.next() {
                        match c {
                            'n' => out.push('\n'),
                            't' => out.push('\t'),
                            '\\' => out.push('\\'),
                            '"' => out.push('"'),
                            _ => {
                                let end = chars.next().map(|(i, _)| i);
                                return Err(TokenizeError::InvalidEscape(i, end));
                            }
                        }
                    } else {
                        return Err(TokenizeError::UnfinishedString(begin.0..));
                    }
                }
                c => {
                    let _ = chars.next();
                    out.push(c);
                }
            }
        } else {
            return Err(TokenizeError::UnfinishedString(begin.0..));
        }
    }
}

fn tok_text(chars: &mut Peekable<CharIndices<'_>>) -> Token {
    let mut out = String::new();
    loop {
        if let Some((_i, c)) = chars.peek().cloned() {
            match c {
                '~' | '(' | ')' | ' ' | '\t' | '\n' | '"' => {
                    return Token::Text(out);
                }
                c => {
                    let _ = chars.next();
                    out.push(c);
                }
            }
        } else {
            return Token::Text(out);
        }
    }
}

fn tok_whitespace(chars: &mut Peekable<CharIndices<'_>>) -> Token {
    let mut out = String::new();
    loop {
        if let Some((_i, c)) = chars.peek().cloned() {
            match c {
                ' ' | '\t' | '\n' => {
                    let _ = chars.next();
                    out.push(c);
                }
                _ => {
                    return Token::Whitespace(out);
                }
            }
        } else {
            return Token::Whitespace(out);
        }
    }
}

#[derive(Debug, PartialEq)]
enum TokenizeError {
    UnfinishedType(std::ops::RangeFrom<usize>),
    UnfinishedString(std::ops::RangeFrom<usize>),
    InvalidEscape(usize, Option<usize>),
}

fn tokenize(s: &str) -> impl Iterator<Item = Result<Token, TokenizeError>> + '_ {
    let mut chars = s.char_indices().peekable();
    std::iter::from_fn(move || loop {
        if let Some((i, c)) = chars.peek().cloned() {
            match c {
                '(' => {
                    let _ = chars.next();
                    return Some(Ok(Token::LParen));
                }
                ')' => {
                    let _ = chars.next();
                    return Some(Ok(Token::RParen));
                }
                '|' => {
                    let _ = chars.next();
                    return Some(Ok(Token::Bar));
                }
                '!' => {
                    let _ = chars.next();
                    return Some(Ok(Token::Exclamation));
                }
                '~' => {
                    let _ = chars.next();
                    return Some(
                        chars
                            .next()
                            .map(|(_, c)| Token::Type(c))
                            .ok_or(TokenizeError::UnfinishedType(i..)),
                    );
                }
                '"' => return Some(tok_string(&mut chars)),
                ' ' | '\t' | '\n' => return Some(Ok(tok_whitespace(&mut chars))),
                _ => return Some(Ok(tok_text(&mut chars))),
            }
        } else {
            return None;
        }
    })
}

fn skip_whitespace(t: &mut &[Token]) {
    loop {
        match *t {
            &[Token::Whitespace(_), ref rest @ ..] => *t = rest,
            _ => return,
        }
    }
}
fn parse_filter_item(t: &mut &[Token]) -> Result<FilterExpression, String> {
    let type_ = match *t {
        &[Token::Type(s), ref rest @ ..] => {
            *t = rest;
            Some(s)
        }
        _ => None,
    };

    skip_whitespace(t);

    let mut output = match *t {
        &[Token::Text(ref s), ref rest @ ..] => {
            *t = rest;
            s.clone()
        }
        o => {
            //TODO: proper error message here
            return Err(format!("Missing filter {:?}", o));
        }
    };

    let mut whitespace = String::new();
    loop {
        match *t {
            &[Token::Whitespace(ref s), ref rest @ ..] => {
                whitespace.push_str(s);
                *t = rest;
            }
            &[Token::Text(ref s), ref rest @ ..] => {
                output.push_str(&whitespace);
                whitespace.clear();
                output.push_str(s);
                *t = rest;
            }
            _ => break,
        };
    }
    match type_ {
        Some('f') => Ok(FilterExpression::Sender(output)),
        Some('b') | None => Ok(FilterExpression::Body(output)),
        Some(o) => Err(format!("Invalid filter type '{}'", o)),
    }
}

fn parse_and(t: &mut &[Token]) -> Result<FilterExpression, String> {
    let mut items = vec![];
    loop {
        skip_whitespace(t);

        match *t {
            &[Token::Type(_) | Token::Text(_), ..] => {
                let item = parse_filter_item(t)?;
                items.push(item);
            }
            &[Token::LParen, ..] => {
                let item = parse_from_tokens(t)?;
                items.push(item);
            }
            &[Token::Exclamation, ref rest @ ..] => {
                *t = rest;
                let item = parse_from_tokens(t)?;
                items.push(FilterExpression::Not(Box::new(item)));
            }
            _ => break,
        }
    }
    match items.len() {
        0 => Err(format!("Need at least one filter")),
        1 => Ok(items.pop().unwrap()),
        _ => Ok(FilterExpression::And(items)),
    }
}

fn parse_from_tokens(t: &mut &[Token]) -> Result<FilterExpression, String> {
    if let &[Token::LParen, ref rest @ ..] = *t {
        *t = rest;
        let v = parse_from_tokens(t)?;
        if let &[Token::RParen, ref rest @ ..] = *t {
            *t = rest;
            return Ok(v);
        } else {
            return Err(format!("Parenthesis not closed"));
        }
    }

    let mut items = vec![];
    loop {
        let item = parse_and(t)?;
        items.push(item);
        if let &[Token::Bar, ref rest @ ..] = *t {
            *t = rest;
        } else {
            break;
        }
    }
    match items.len() {
        0 => Err(format!("Need at least one filter")),
        1 => Ok(items.pop().unwrap()),
        _ => Ok(FilterExpression::Or(items)),
    }
}

impl FilterExpression {
    fn build(self) -> Result<Filter, String> {
        Ok(match self {
            FilterExpression::Sender(sender) => {
                Filter::Sender(Regex::new(&sender).map_err(|e| e.to_string())?)
            }
            FilterExpression::Body(body) => {
                Filter::Body(Regex::new(&body).map_err(|e| e.to_string())?)
            }
            FilterExpression::Not(v) => Filter::Not(Box::new(v.build()?)),
            FilterExpression::And(v) => Filter::And(
                v.into_iter()
                    .map(FilterExpression::build)
                    .collect::<Result<_, _>>()?,
            ),
            FilterExpression::Or(v) => Filter::Or(
                v.into_iter()
                    .map(FilterExpression::build)
                    .collect::<Result<_, _>>()?,
            ),
        })
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_tokenize() {
        assert_eq!(tokenize("").collect::<Result<Vec<_>, _>>(), Ok(vec![]));
        assert_eq!(
            tokenize("()~b").collect::<Result<Vec<_>, _>>(),
            Ok(vec![Token::LParen, Token::RParen, Token::Type('b')])
        );
        assert_eq!(
            tokenize("~b asdf").collect::<Result<Vec<_>, _>>(),
            Ok(vec![
                Token::Type('b'),
                Token::Whitespace(" ".to_owned()),
                Token::Text("asdf".to_owned())
            ])
        );
        assert_eq!(
            tokenize("|asdf\"~\"\t ").collect::<Result<Vec<_>, _>>(),
            Ok(vec![
                Token::Bar,
                Token::Text("asdf".to_owned()),
                Token::Text("~".to_owned()),
                Token::Whitespace("\t ".to_owned()),
            ])
        );
        assert_eq!(
            tokenize("\"\\\"\"\t ").collect::<Result<Vec<_>, _>>(),
            Ok(vec![
                Token::Text("\"".to_owned()),
                Token::Whitespace("\t ".to_owned()),
            ])
        );
    }
    #[test]
    fn test_parse_default() {
        assert_eq!(
            FilterExpression::parse("foo"),
            Ok(FilterExpression::Body("foo".to_owned()))
        );
        assert_eq!(
            FilterExpression::parse("  foo "),
            Ok(FilterExpression::Body("foo".to_owned()))
        );
        assert_eq!(
            FilterExpression::parse(" \"  foo \""),
            Ok(FilterExpression::Body("  foo ".to_owned()))
        );
    }
    #[test]
    fn test_parse_quote() {
        assert_eq!(
            FilterExpression::parse("\"foo\""),
            Ok(FilterExpression::Body("foo".to_owned()))
        );
        assert_eq!(
            FilterExpression::parse("\"~foo\""),
            Ok(FilterExpression::Body("~foo".to_owned()))
        );
        assert_eq!(
            FilterExpression::parse("~b \"~foo\""),
            Ok(FilterExpression::Body("~foo".to_owned()))
        );
        assert_eq!(
            FilterExpression::parse("~b \"fo!o\""),
            Ok(FilterExpression::Body("fo!o".to_owned()))
        );
        assert_eq!(
            FilterExpression::parse(r#"~b "\"" "#),
            Ok(FilterExpression::Body("\"".to_owned()))
        );
    }
    #[test]
    fn test_parse_body() {
        assert_eq!(
            FilterExpression::parse("~b foo"),
            Ok(FilterExpression::Body("foo".to_owned()))
        );
        assert_eq!(
            FilterExpression::parse("  ~bfoo "),
            Ok(FilterExpression::Body("foo".to_owned()))
        );
        assert_eq!(
            FilterExpression::parse(" ~b\"  foo \""),
            Ok(FilterExpression::Body("  foo ".to_owned()))
        );
    }
    #[test]
    fn test_parse_sender() {
        assert_eq!(
            FilterExpression::parse("~f foo"),
            Ok(FilterExpression::Sender("foo".to_owned()))
        );
        assert_eq!(
            FilterExpression::parse("  ~ffoo "),
            Ok(FilterExpression::Sender("foo".to_owned()))
        );
        assert_eq!(
            FilterExpression::parse(" ~f\"  foo \""),
            Ok(FilterExpression::Sender("  foo ".to_owned()))
        );
    }
    #[test]
    fn test_parse_and() {
        assert_eq!(
            FilterExpression::parse("bla ~f foo"),
            Ok(FilterExpression::And(vec![
                FilterExpression::Body("bla".to_owned()),
                FilterExpression::Sender("foo".to_owned())
            ]))
        );
        assert_eq!(
            FilterExpression::parse("~f bla ~f foo"),
            Ok(FilterExpression::And(vec![
                FilterExpression::Sender("bla".to_owned()),
                FilterExpression::Sender("foo".to_owned())
            ]))
        );
        assert_eq!(
            FilterExpression::parse("~f bla ~f \"foo\" ~bbar oi"),
            Ok(FilterExpression::And(vec![
                FilterExpression::Sender("bla".to_owned()),
                FilterExpression::Sender("foo".to_owned()),
                FilterExpression::Body("bar oi".to_owned())
            ]))
        );
    }

    #[test]
    fn test_parse_or() {
        assert_eq!(
            FilterExpression::parse("bla | ~f foo"),
            Ok(FilterExpression::Or(vec![
                FilterExpression::Body("bla".to_owned()),
                FilterExpression::Sender("foo".to_owned())
            ]))
        );
        assert_eq!(
            FilterExpression::parse("~f bla | ~f foo"),
            Ok(FilterExpression::Or(vec![
                FilterExpression::Sender("bla".to_owned()),
                FilterExpression::Sender("foo".to_owned())
            ]))
        );
        assert_eq!(
            FilterExpression::parse("~f bla bli | ~f \"foo\" | ~bbar oi"),
            Ok(FilterExpression::Or(vec![
                FilterExpression::Sender("bla bli".to_owned()),
                FilterExpression::Sender("foo".to_owned()),
                FilterExpression::Body("bar oi".to_owned())
            ]))
        );
    }
    #[test]
    fn test_parse_not() {
        assert_eq!(
            FilterExpression::parse("!foo"),
            Ok(FilterExpression::Not(Box::new(FilterExpression::Body(
                "foo".to_owned()
            ))))
        );
        assert_eq!(
            FilterExpression::parse("bla !foo"),
            Ok(FilterExpression::And(vec![
                FilterExpression::Body("bla".to_owned()),
                FilterExpression::Not(Box::new(FilterExpression::Body("foo".to_owned())))
            ]))
        );
        assert_eq!(
            FilterExpression::parse("~bbla !~ffoo"),
            Ok(FilterExpression::And(vec![
                FilterExpression::Body("bla".to_owned()),
                FilterExpression::Not(Box::new(FilterExpression::Sender("foo".to_owned())))
            ]))
        );
        assert_eq!(
            FilterExpression::parse("~bbla !(~ffoo | ~bbli)"),
            Ok(FilterExpression::And(vec![
                FilterExpression::Body("bla".to_owned()),
                FilterExpression::Not(Box::new(FilterExpression::Or(vec![
                    FilterExpression::Sender("foo".to_owned()),
                    FilterExpression::Body("bli".to_owned()),
                ])))
            ]))
        );
    }
    #[test]
    fn test_parse_complex() {
        assert_eq!(
            FilterExpression::parse("~f \"bla ~!()\"bli"),
            Ok(FilterExpression::Sender("bla ~!()bli".to_owned()))
        );
        assert_eq!(
            FilterExpression::parse("~f bla | (asdf ~b foo)"),
            Ok(FilterExpression::Or(vec![
                FilterExpression::Sender("bla".to_owned()),
                FilterExpression::And(vec![
                    FilterExpression::Body("asdf".to_owned()),
                    FilterExpression::Body("foo".to_owned()),
                ])
            ]))
        );
    }
}
