use std::{iter::Peekable, str::CharIndices};

use matrix_sdk::ruma::events::AnySyncMessageEvent;

use crate::timeline::Event;

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Filter {
    Sender(String),
    Body(String),
    Not(Box<Filter>),
    And(Vec<Filter>),
    Or(Vec<Filter>),
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
                    if let Some((j, c)) = chars.next() {
                        match c {
                            'n' => out.push('\n'),
                            't' => out.push('\t'),
                            '\\' => out.push('\\'),
                            '"' => out.push('"'),
                            _ => return Err(TokenizeError::InvalidEscape(i..=j)), //TODO: not sure if this works for umlauts etc
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
    InvalidEscape(std::ops::RangeInclusive<usize>),
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
fn parse_filter_item(t: &mut &[Token]) -> Result<Filter, String> {
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
        Some('f') => Ok(Filter::Sender(output)),
        Some('b') | None => Ok(Filter::Body(output)),
        Some(o) => Err(format!("Invalid filter type '{}'", o)),
    }
}

fn parse_and(t: &mut &[Token]) -> Result<Filter, String> {
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
                items.push(Filter::Not(Box::new(item)));
            }
            _ => break,
        }
    }
    match items.len() {
        0 => Err(format!("Need at least one filter")),
        1 => Ok(items.pop().unwrap()),
        _ => Ok(Filter::And(items)),
    }
}

fn parse_from_tokens(t: &mut &[Token]) -> Result<Filter, String> {
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
        _ => Ok(Filter::Or(items)),
    }
}

impl Filter {
    pub fn parse(s: &str) -> Result<Self, String> {
        let tokens = tokenize(s).collect::<Result<Vec<_>, TokenizeError>>();
        let tokens = match tokens {
            Ok(t) => t,
            Err(TokenizeError::InvalidEscape(r)) => {
                return Err(format!("Invalid escape expression: {}", &s[r]))
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
        Ok(f)
    }
    pub fn matches(&self, event: &Event) -> bool {
        match self {
            Filter::Sender(sender) => event.sender().as_str().contains(sender),
            Filter::Body(body) => {
                if let Event::Message(AnySyncMessageEvent::RoomMessage(m)) = event {
                    crate::tui_app::tui::messages::strip_body(m.content.body()).contains(body)
                } else {
                    false
                }
            }
            Filter::Not(v) => !v.matches(event),
            Filter::And(v) => v.iter().all(|f| f.matches(event)),
            Filter::Or(v) => v.iter().any(|f| f.matches(event)),
        }
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
        assert_eq!(Filter::parse("foo"), Ok(Filter::Body("foo".to_owned())));
        assert_eq!(Filter::parse("  foo "), Ok(Filter::Body("foo".to_owned())));
        assert_eq!(
            Filter::parse(" \"  foo \""),
            Ok(Filter::Body("  foo ".to_owned()))
        );
    }
    #[test]
    fn test_parse_quote() {
        //assert_eq!(Filter::parse("\"foo\""), Ok(Filter::Body("foo".to_owned())));
        //assert_eq!(
        //    Filter::parse("\"~foo\""),
        //    Ok(Filter::Body("~foo".to_owned()))
        //);
        //assert_eq!(
        //    Filter::parse("~b \"~foo\""),
        //    Ok(Filter::Body("~foo".to_owned()))
        //);
        //assert_eq!(
        //    Filter::parse("~b \"fo!o\""),
        //    Ok(Filter::Body("fo!o".to_owned()))
        //);
        assert_eq!(
            Filter::parse(r#"~b "\"" "#),
            Ok(Filter::Body("\"".to_owned()))
        );
    }
    #[test]
    fn test_parse_body() {
        assert_eq!(Filter::parse("~b foo"), Ok(Filter::Body("foo".to_owned())));
        assert_eq!(
            Filter::parse("  ~bfoo "),
            Ok(Filter::Body("foo".to_owned()))
        );
        assert_eq!(
            Filter::parse(" ~b\"  foo \""),
            Ok(Filter::Body("  foo ".to_owned()))
        );
    }
    #[test]
    fn test_parse_sender() {
        assert_eq!(
            Filter::parse("~f foo"),
            Ok(Filter::Sender("foo".to_owned()))
        );
        assert_eq!(
            Filter::parse("  ~ffoo "),
            Ok(Filter::Sender("foo".to_owned()))
        );
        assert_eq!(
            Filter::parse(" ~f\"  foo \""),
            Ok(Filter::Sender("  foo ".to_owned()))
        );
    }
    #[test]
    fn test_parse_and() {
        //assert_eq!(
        //    Filter::parse("bla ~f foo"),
        //    Ok(Filter::And(vec![
        //        Filter::Body("bla".to_owned()),
        //        Filter::Sender("foo".to_owned())
        //    ]))
        //);
        //assert_eq!(
        //    Filter::parse("~f bla ~f foo"),
        //    Ok(Filter::And(vec![
        //        Filter::Sender("bla".to_owned()),
        //        Filter::Sender("foo".to_owned())
        //    ]))
        //);
        assert_eq!(
            Filter::parse("~f bla ~f \"foo\" ~bbar oi"),
            Ok(Filter::And(vec![
                Filter::Sender("bla".to_owned()),
                Filter::Sender("foo".to_owned()),
                Filter::Body("bar oi".to_owned())
            ]))
        );
    }

    #[test]
    fn test_parse_or() {
        assert_eq!(
            Filter::parse("bla | ~f foo"),
            Ok(Filter::Or(vec![
                Filter::Body("bla".to_owned()),
                Filter::Sender("foo".to_owned())
            ]))
        );
        assert_eq!(
            Filter::parse("~f bla | ~f foo"),
            Ok(Filter::Or(vec![
                Filter::Sender("bla".to_owned()),
                Filter::Sender("foo".to_owned())
            ]))
        );
        assert_eq!(
            Filter::parse("~f bla bli | ~f \"foo\" | ~bbar oi"),
            Ok(Filter::Or(vec![
                Filter::Sender("bla bli".to_owned()),
                Filter::Sender("foo".to_owned()),
                Filter::Body("bar oi".to_owned())
            ]))
        );
    }
    #[test]
    fn test_parse_not() {
        assert_eq!(
            Filter::parse("!foo"),
            Ok(Filter::Not(Box::new(Filter::Body("foo".to_owned()))))
        );
        assert_eq!(
            Filter::parse("bla !foo"),
            Ok(Filter::And(vec![
                Filter::Body("bla".to_owned()),
                Filter::Not(Box::new(Filter::Body("foo".to_owned())))
            ]))
        );
        assert_eq!(
            Filter::parse("~bbla !~ffoo"),
            Ok(Filter::And(vec![
                Filter::Body("bla".to_owned()),
                Filter::Not(Box::new(Filter::Sender("foo".to_owned())))
            ]))
        );
        assert_eq!(
            Filter::parse("~bbla !(~ffoo | ~bbli)"),
            Ok(Filter::And(vec![
                Filter::Body("bla".to_owned()),
                Filter::Not(Box::new(Filter::Or(vec![
                    Filter::Sender("foo".to_owned()),
                    Filter::Body("bli".to_owned()),
                ])))
            ]))
        );
    }
    #[test]
    fn test_parse_complex() {
        assert_eq!(
            Filter::parse("~f \"bla ~!()\"bli"),
            Ok(Filter::Sender("bla ~!()bli".to_owned()))
        );
        assert_eq!(
            Filter::parse("~f bla | (asdf ~b foo)"),
            Ok(Filter::Or(vec![
                Filter::Sender("bla".to_owned()),
                Filter::And(vec![
                    Filter::Body("asdf".to_owned()),
                    Filter::Body("foo".to_owned()),
                ])
            ]))
        );
    }
}
