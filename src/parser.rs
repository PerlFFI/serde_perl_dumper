//! A parser for the subset of Perl that `Data::Dumper` (and hand-written
//! literals of the same shape) produce.
//!
//! The lexical primitives here (`ws`, `quoted`, `number`, ...) are shared with
//! the streaming deserializer in `de::stream`, which reads values straight
//! from the input without building a [`Scalar`] tree.

use std::collections::BTreeMap;

use beef::lean::Cow;
use memchr::{memchr, memchr2, memchr3};
use winnow::combinator::{alt, cut_err, delimited, not, opt, peek, repeat, terminated};
use winnow::error::{ContextError, ErrMode, StrContext, StrContextValue};
use winnow::prelude::*;
use winnow::stream::Stateful;
use winnow::token::{any, one_of, take_while};

use crate::error::{Error, Result};

/// Maximum nesting depth, to avoid blowing the stack on hostile input.
pub(crate) const MAX_DEPTH: usize = 128;

#[derive(Debug, Clone, PartialEq)]
pub enum Scalar<'a> {
    Undefined,
    Int(i64),
    Float(f64),
    String(Cow<'a, str>),
    Reference(Box<Reference<'a>>),
    /// `bless( VALUE, 'Class' )`
    Blessed(Cow<'a, str>, Box<Scalar<'a>>),
    /// `qr/pattern/flags`
    Regex {
        pattern: Cow<'a, str>,
        flags: Cow<'a, str>,
    },
    /// A typeglob such as `*::STDOUT`; the name is discarded.
    Glob,
    /// `$VAR1->{'a'}[0]`: a reference to another part of the dump. These are
    /// left as-is by [`parse`] and [`parse_all`]; see [`resolve_backrefs`].
    BackRef(BackRef<'a>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hash<'a>(pub BTreeMap<Cow<'a, str>, Scalar<'a>>);

#[derive(Debug, Clone, PartialEq)]
pub struct Array<'a>(pub Vec<Scalar<'a>>);

#[derive(Debug, Clone, PartialEq)]
pub enum Reference<'a> {
    Hash(Box<Hash<'a>>),
    Array(Box<Array<'a>>),
    Scalar(Box<Scalar<'a>>),
}

/// A `$VAR1->{'a'}[0]` style reference to another part of the dump.
#[derive(Debug, Clone, PartialEq)]
pub struct BackRef<'a> {
    /// The variable name without the sigil, e.g. `VAR1`.
    pub var: Cow<'a, str>,
    pub path: Vec<Step<'a>>,
}

/// One step of a back-reference path.
#[derive(Debug, Clone, PartialEq)]
pub enum Step<'a> {
    /// `{'key'}`
    Key(Cow<'a, str>),
    /// `[0]`
    Index(usize),
    /// `${ ... }`
    Deref,
}

/// Parse a single value. Accepts either a bare expression (`{ a => 1 }`) or
/// Data::Dumper's `$VAR1 = ...;` form. If several variables are present the
/// first is returned; use [`parse_all`] to get them all.
///
/// Back-references are not resolved; see [`resolve_backrefs`].
pub fn parse(input: &str) -> Result<Scalar<'_>> {
    let mut all = parse_all(input)?;
    Ok(all.remove(0))
}

/// Parse every `$VARn = ...;` statement (or a single bare expression).
///
/// Back-references such as `$VAR1->{'a'}` are returned as
/// [`Scalar::BackRef`]; call [`resolve_backrefs`] to replace them with copies.
pub fn parse_all(src: &str) -> Result<Vec<Scalar<'_>>> {
    let mut i = Input {
        input: src,
        state: State::default(),
    };
    let result = document(&mut i).and_then(|vars| {
        ws(&mut i)?;
        if i.input.is_empty() {
            Ok(vars)
        } else {
            fail("unexpected trailing input")
        }
    });
    result.map_err(|e| error_at(src, src.len() - i.input.len(), e))
}

/// Parse a file made of `$name = value;` statements, as Perl config files
/// often are, keeping the variable names. Statements appear in source
/// order; a name assigned twice appears twice. An empty file (or one with
/// only comments) is fine and gives an empty list.
///
/// `my`/`our`/`local` prefixes are accepted. Values can be anything
/// [`parse`] accepts.
pub fn parse_assignments(src: &str) -> Result<Vec<(Cow<'_, str>, Scalar<'_>)>> {
    let mut i = Input {
        input: src,
        state: State::default(),
    };
    assignments(&mut i).map_err(|e| error_at(src, src.len() - i.input.len(), e))
}

#[derive(Debug, Default, Clone)]
pub(crate) struct State {
    pub depth: usize,
}

pub(crate) type Input<'a> = Stateful<&'a str, State>;
pub(crate) type PResult<O> = winnow::ModalResult<O>;

pub(crate) fn error_at(src: &str, offset: usize, e: ErrMode<ContextError>) -> Error {
    let e = e.into_inner().unwrap_or_default();
    let mut expected = Vec::new();
    let mut label = None;
    for c in e.context() {
        match c {
            StrContext::Label(l) => label = Some(l.to_string()),
            StrContext::Expected(v) => expected.push(v.to_string()),
            _ => {}
        }
    }
    let msg = match (label, expected.is_empty()) {
        (Some(l), _) => l,
        (None, false) => format!("expected {}", expected.join(" or ")),
        (None, true) => "syntax error".to_string(),
    };
    let before = &src[..offset];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    Error::Parse(format!("{msg} at line {line}, column {col}"))
}

pub(crate) fn fail<O>(msg: &'static str) -> PResult<O> {
    let mut e = ContextError::new();
    e.push(StrContext::Label(msg));
    Err(ErrMode::Cut(e))
}

fn backtrack<O>() -> PResult<O> {
    Err(ErrMode::Backtrack(ContextError::new()))
}

fn expected(what: &'static str) -> StrContext {
    StrContext::Expected(StrContextValue::Description(what))
}

/// Skip whitespace and `#` comments.
pub(crate) fn ws(i: &mut Input<'_>) -> PResult<()> {
    let b = i.input.as_bytes();
    let mut p = 0;
    loop {
        while p < b.len() && b[p].is_ascii_whitespace() {
            p += 1;
        }
        if p < b.len() && b[p] == b'#' {
            p += memchr(b'\n', &b[p..]).unwrap_or(b.len() - p);
        } else {
            break;
        }
    }
    i.input = &i.input[p..];
    Ok(())
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == ':'
}

/// A keyword: the literal text, not followed by more identifier characters.
pub(crate) fn kw<'a>(w: &'static str) -> impl Parser<Input<'a>, &'a str, ErrMode<ContextError>> {
    terminated(w, not(one_of(is_word)))
}

pub(crate) fn ident<'a>(i: &mut Input<'a>) -> PResult<&'a str> {
    take_while(1.., is_word)
        .context(expected("identifier"))
        .parse_next(i)
}

/// `my`, `our` or `local`, if present.
pub(crate) fn decl(i: &mut Input<'_>) -> PResult<()> {
    opt((
        alt((kw("my"), kw("our"), kw("local"))),
        winnow::ascii::multispace1,
    ))
    .void()
    .parse_next(i)
}

/// `$name =`, returning the name without the sigil.
pub(crate) fn assign_head<'a>(i: &mut Input<'a>) -> PResult<&'a str> {
    decl(i)?;
    ('$', ident, ws, '=')
        .map(|(_, name, _, _)| name)
        .context(expected("'$name ='"))
        .parse_next(i)
}

/// Does the input (after whitespace) start a `$VARn = ...;` statement?
pub(crate) fn at_assignment(i: &mut Input<'_>) -> PResult<bool> {
    Ok(opt(peek((decl, '$'))).parse_next(i)?.is_some())
}

fn document<'a>(i: &mut Input<'a>) -> PResult<Vec<Scalar<'a>>> {
    ws(i)?;
    if i.input.is_empty() {
        return fail("empty input");
    }
    if !at_assignment(i)? {
        return Ok(vec![value(i)?]);
    }
    Ok(assignments(i)?.into_iter().map(|(_, v)| v).collect())
}

/// Zero or more `$name = value;` statements up to the end of input.
fn assignments<'a>(i: &mut Input<'a>) -> PResult<Vec<(Cow<'a, str>, Scalar<'a>)>> {
    let mut out = Vec::new();
    loop {
        ws(i)?;
        if i.input.is_empty() {
            return Ok(out);
        }
        let name = assign_head(i)?;
        let v = value(i)?;
        ws(i)?;
        if opt(';').parse_next(i)?.is_none() && !i.input.is_empty() {
            return fail("expected ';'");
        }
        out.push((Cow::borrowed(name), v));
    }
}

fn value<'a>(i: &mut Input<'a>) -> PResult<Scalar<'a>> {
    ws(i)?;
    i.state.depth += 1;
    if i.state.depth > MAX_DEPTH {
        return fail("nesting too deep");
    }
    let v = value_inner(i);
    i.state.depth -= 1;
    v
}

fn value_inner<'a>(i: &mut Input<'a>) -> PResult<Scalar<'a>> {
    let Some(&c) = i.input.as_bytes().first() else {
        return fail("unexpected end of input");
    };
    match c {
        b'{' => hash(i),
        b'[' => array(i),
        b'\\' => {
            i.input = &i.input[1..];
            let v = value(i)?;
            Ok(Scalar::Reference(Box::new(Reference::Scalar(Box::new(v)))))
        }
        b'\'' | b'"' => string_literal(i).map(Scalar::String),
        b'$' => backref(i).map(Scalar::BackRef),
        b'*' => glob(i).map(|()| Scalar::Glob),
        b'-' | b'+' | b'.' | b'0'..=b'9' => number(i).map(|n| match n {
            Num::Int(n) => Scalar::Int(n),
            Num::Float(f) => Scalar::Float(f),
        }),
        _ => {
            if opt(kw("undef")).parse_next(i)?.is_some() {
                Ok(Scalar::Undefined)
            } else if opt(kw("bless")).parse_next(i)?.is_some() {
                bless(i)
            } else if opt(kw("do")).parse_next(i)?.is_some() {
                do_open(i)?;
                let v = value(i)?;
                do_close(i)?;
                Ok(Scalar::Reference(Box::new(Reference::Scalar(Box::new(v)))))
            } else if opt(kw("qr")).parse_next(i)?.is_some() {
                regex(i)
            } else if let Some(s) = opt(string_literal).parse_next(i)? {
                Ok(Scalar::String(s))
            } else {
                fail("unexpected character")
            }
        }
    }
}

pub(crate) fn comma(i: &mut Input<'_>) -> PResult<()> {
    alt((','.void(), "=>".void())).parse_next(i)
}

pub(crate) fn close(i: &mut Input<'_>, c: char) -> PResult<()> {
    ws(i)?;
    cut_err(
        c.void()
            .context(StrContext::Expected(StrContextValue::CharLiteral(c))),
    )
    .parse_next(i)
}

fn array<'a>(i: &mut Input<'a>) -> PResult<Scalar<'a>> {
    '['.parse_next(i)?;
    let mut items = Vec::new();
    loop {
        ws(i)?;
        if opt(']').parse_next(i)?.is_some() {
            break;
        }
        items.push(value(i)?);
        ws(i)?;
        if opt(comma).parse_next(i)?.is_none() {
            close(i, ']')?;
            break;
        }
    }
    Ok(Scalar::Reference(Box::new(Reference::Array(Box::new(
        Array(items),
    )))))
}

fn hash<'a>(i: &mut Input<'a>) -> PResult<Scalar<'a>> {
    '{'.parse_next(i)?;
    let mut map = BTreeMap::new();
    loop {
        ws(i)?;
        if opt('}').parse_next(i)?.is_some() {
            break;
        }
        let k = key(i)?;
        let v = value(i)?;
        map.insert(k, v);
        ws(i)?;
        if opt(comma).parse_next(i)?.is_none() {
            close(i, '}')?;
            break;
        }
    }
    Ok(Scalar::Reference(Box::new(Reference::Hash(Box::new(
        Hash(map),
    )))))
}

/// A hash key followed by its `,` or `=>`.
pub(crate) fn key<'a>(i: &mut Input<'a>) -> PResult<Cow<'a, str>> {
    let k = key_token(i)?;
    ws(i)?;
    cut_err(comma.context(expected("',' or '=>' after hash key"))).parse_next(i)?;
    Ok(k)
}

/// Words that start an expression and so are only keys when followed by `=>`.
const OP_WORDS: [&str; 7] = ["q", "qq", "qr", "undef", "bless", "do", "my"];

fn bareword_key<'a>(i: &mut Input<'a>) -> PResult<&'a str> {
    let word = (
        opt('-'),
        one_of(|c: char| c.is_alphabetic() || c == '_'),
        take_while(0.., is_word),
    )
        .take()
        .parse_next(i)?;
    if OP_WORDS.contains(&word) && opt(peek((ws, "=>"))).parse_next(i)?.is_none() {
        return backtrack();
    }
    Ok(word)
}

/// A hash key on its own: a bareword, number, or string literal.
pub(crate) fn key_token<'a>(i: &mut Input<'a>) -> PResult<Cow<'a, str>> {
    ws(i)?;
    if let Some(w) = opt(bareword_key).parse_next(i)? {
        return Ok(Cow::borrowed(w));
    }
    match i.input.as_bytes().first() {
        Some(b'-' | b'+' | b'.' | b'0'..=b'9') => Ok(match number(i)? {
            Num::Int(n) => n.to_string().into(),
            Num::Float(f) => f.to_string().into(),
        }),
        _ => {
            if opt(kw("undef")).parse_next(i)?.is_some() {
                return Ok(Cow::borrowed(""));
            }
            match opt(string_literal).parse_next(i)? {
                Some(s) => Ok(s),
                None => fail("invalid hash key"),
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Mode {
    /// `'...'`, `q(...)`: only `\\` and the delimiters can be escaped.
    Single,
    /// `"..."`, `qq(...)`: full escape sequences.
    Double,
    /// Escapes are kept verbatim (regex patterns).
    Raw,
}

fn next_stop(b: &[u8], open: u8, close: u8) -> Option<usize> {
    if open == close {
        memchr2(b'\\', close, b)
    } else {
        memchr3(b'\\', open, close, b)
    }
}

/// Read up to the matching close delimiter (the open one is already
/// consumed), honouring nesting for bracket pairs. Both delimiters must be
/// ASCII. Borrows from the input unless the string needs rewriting.
pub(crate) fn quoted<'a>(
    i: &mut Input<'a>,
    open: char,
    close: char,
    mode: Mode,
) -> PResult<Cow<'a, str>> {
    debug_assert!(open.is_ascii() && close.is_ascii());
    let (o, c) = (open as u8, close as u8);
    let src: &'a str = i.input;
    let b = src.as_bytes();
    let Some(n) = next_stop(b, o, c) else {
        return fail("unterminated string");
    };
    // Fast path: no escapes or nesting, so the result is a slice of the input.
    if b[n] == c {
        i.input = &src[n + 1..];
        return Ok(Cow::borrowed(&src[..n]));
    }
    let mut out = String::with_capacity(n + 16);
    out.push_str(&src[..n]);
    let mut pos = n;
    let mut nest = 0usize;
    loop {
        // b[pos] is a delimiter or a backslash
        let ch = b[pos];
        pos += 1;
        if ch == c && nest == 0 {
            i.input = &src[pos..];
            return Ok(out.into());
        }
        if ch != b'\\' {
            if o != c && ch == o {
                nest += 1;
            } else {
                nest -= 1;
            }
            out.push(ch as char);
        } else {
            match mode {
                Mode::Raw => {
                    out.push('\\');
                    let Some(next) = src[pos..].chars().next() else {
                        return fail("unterminated string");
                    };
                    out.push(next);
                    pos += next.len_utf8();
                }
                Mode::Single => match b.get(pos) {
                    Some(&n) if n == b'\\' || n == o || n == c => {
                        out.push(n as char);
                        pos += 1;
                    }
                    _ => out.push('\\'),
                },
                Mode::Double => {
                    i.input = &src[pos..];
                    if i.input.is_empty() {
                        return fail("unterminated string");
                    }
                    out.push(double_escape(i)?);
                    pos = src.len() - i.input.len();
                }
            }
        }
        match next_stop(&b[pos..], o, c) {
            Some(m) => {
                out.push_str(&src[pos..pos + m]);
                pos += m;
            }
            None => return fail("unterminated string"),
        }
    }
}

/// The part of a double-quoted escape after the backslash. `$` and `@` are
/// never interpolated; Data::Dumper escapes them anyway.
fn double_escape(i: &mut Input<'_>) -> PResult<char> {
    let c = any.parse_next(i)?;
    let code = |digits: &str, radix| {
        char::from_u32(u32::from_str_radix(digits.trim(), radix).unwrap_or(0)).unwrap_or('\u{FFFD}')
    };
    Ok(match c {
        'n' => '\n',
        'r' => '\r',
        't' => '\t',
        'f' => '\x0C',
        'b' => '\x08',
        'a' => '\x07',
        'e' => '\x1B',
        'x' => {
            let hex: &str = alt((
                delimited('{', winnow::token::take_till(0.., '}'), '}'),
                take_while(0..=2, |c: char| c.is_ascii_hexdigit()),
            ))
            .parse_next(i)?;
            code(hex, 16)
        }
        '0'..='7' => {
            let rest = take_while(0..=2, '0'..='7').parse_next(i)?;
            code(&format!("{c}{rest}"), 8)
        }
        other => other,
    })
}

/// Any string literal: `'..'`, `".."`, `q(..)` or `qq(..)`. Fails with a
/// backtrack error if the input doesn't start one.
pub(crate) fn string_literal<'a>(i: &mut Input<'a>) -> PResult<Cow<'a, str>> {
    match i.input.as_bytes().first() {
        Some(b'\'') => {
            i.input = &i.input[1..];
            quoted(i, '\'', '\'', Mode::Single)
        }
        Some(b'"') => {
            i.input = &i.input[1..];
            quoted(i, '"', '"', Mode::Double)
        }
        _ => {
            let mode = if opt(kw("qq")).parse_next(i)?.is_some() {
                Mode::Double
            } else if opt(kw("q")).parse_next(i)?.is_some() {
                Mode::Single
            } else {
                return backtrack();
            };
            let (open, close) = open_delim(i)?;
            quoted(i, open, close, mode)
        }
    }
}

fn open_delim(i: &mut Input<'_>) -> PResult<(char, char)> {
    ws(i)?;
    let open = one_of(|c: char| c.is_ascii_punctuation() && c != '_')
        .context(expected("quote delimiter"))
        .parse_next(i)?;
    let close = match open {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '<' => '>',
        o => o,
    };
    Ok((open, close))
}

/// The pattern and flags of a `qr//` (after the `qr` keyword).
pub(crate) fn regex_body<'a>(i: &mut Input<'a>) -> PResult<(Cow<'a, str>, &'a str)> {
    let (open, close) = open_delim(i)?;
    let pattern = quoted(i, open, close, Mode::Raw)?;
    let flags = take_while(0.., |c: char| c.is_ascii_alphabetic()).parse_next(i)?;
    Ok((pattern, flags))
}

fn regex<'a>(i: &mut Input<'a>) -> PResult<Scalar<'a>> {
    let (pattern, flags) = regex_body(i)?;
    Ok(Scalar::Regex {
        pattern,
        flags: Cow::borrowed(flags),
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Num {
    Int(i64),
    Float(f64),
}

/// A Perl numeric literal: decimal, float with exponent, `0x`/`0b`/octal, with
/// optional `_` separators and sign. Scans bytes directly and only allocates
/// for the rare forms (separators, overflow).
pub(crate) fn number(i: &mut Input<'_>) -> PResult<Num> {
    let src = i.input;
    let b = src.as_bytes();
    let at = |p: usize| b.get(p).copied().unwrap_or(0);
    let mut p = 0;
    let neg = at(0) == b'-';
    if matches!(at(0), b'+' | b'-') {
        p = 1;
    }
    let sign = |n: i64| if neg { -n } else { n };

    // 0x / 0b
    if at(p) == b'0' && matches!(at(p + 1), b'x' | b'X' | b'b' | b'B') {
        let radix = if matches!(at(p + 1), b'x' | b'X') {
            16
        } else {
            2
        };
        let start = p + 2;
        let mut e = start;
        while e < b.len() && (b[e] == b'_' || (b[e] as char).is_digit(radix)) {
            e += 1;
        }
        if e > start {
            let digits = src[start..e].replace('_', "");
            return match i64::from_str_radix(&digits, radix) {
                Ok(n) => {
                    i.input = &src[e..];
                    Ok(Num::Int(sign(n)))
                }
                Err(_) => backtrack(),
            };
        }
    }

    let int_start = p;
    while at(p).is_ascii_digit() || at(p) == b'_' {
        p += 1;
    }
    let int_end = p;
    let mut float = false;
    let mut digits = int_end - int_start;
    if at(p) == b'.' {
        float = true;
        p += 1;
        let frac_start = p;
        while at(p).is_ascii_digit() || at(p) == b'_' {
            p += 1;
        }
        digits += p - frac_start;
    }
    if digits == 0 {
        return backtrack();
    }
    if matches!(at(p), b'e' | b'E') {
        let mut q = p + 1;
        if matches!(at(q), b'+' | b'-') {
            q += 1;
        }
        if at(q).is_ascii_digit() {
            while at(q).is_ascii_digit() {
                q += 1;
            }
            float = true;
            p = q;
        }
    }

    let text = &src[..p];
    let int_digits = &b[int_start..int_end];
    let parsed = if !float && !int_digits.contains(&b'_') {
        if int_digits.len() > 1 && int_digits[0] == b'0' {
            i64::from_str_radix(&src[int_start..int_end], 8)
                .ok()
                .map(|n| Num::Int(sign(n)))
        } else if int_digits.len() <= 18 {
            let n = int_digits
                .iter()
                .fold(0i64, |n, d| n * 10 + i64::from(d - b'0'));
            Some(Num::Int(sign(n)))
        } else {
            text.parse::<i64>()
                .ok()
                .map(Num::Int)
                .or_else(|| text.parse::<f64>().ok().map(Num::Float))
        }
    } else {
        slow_number(text, int_start, int_end, float)
    };
    match parsed {
        Some(n) => {
            i.input = &src[p..];
            Ok(n)
        }
        None => backtrack(),
    }
}

/// Numbers with `_` separators.
fn slow_number(text: &str, int_start: usize, int_end: usize, float: bool) -> Option<Num> {
    let clean = text.replace('_', "");
    if float {
        return clean.parse::<f64>().ok().map(Num::Float);
    }
    let int_len = int_end - int_start - text[int_start..int_end].matches('_').count();
    let digits = &clean[clean.len() - int_len..];
    if int_len > 1 && digits.starts_with('0') {
        let n = i64::from_str_radix(digits, 8).ok()?;
        return Some(Num::Int(if clean.starts_with('-') { -n } else { n }));
    }
    clean
        .parse::<i64>()
        .ok()
        .map(Num::Int)
        .or_else(|| clean.parse::<f64>().ok().map(Num::Float))
}

/// `*::NAME` or `*{'name'}`. The name is discarded.
pub(crate) fn glob(i: &mut Input<'_>) -> PResult<()> {
    '*'.parse_next(i)?;
    if opt('{').parse_next(i)?.is_some() {
        ws(i)?;
        if string_literal(i).is_err() {
            return fail("expected glob name");
        }
        close(i, '}')
    } else {
        ident(i).map(|_| ())
    }
}

/// After the `bless` keyword: the opening `(` if any. Returns whether a
/// paren was seen.
pub(crate) fn bless_open(i: &mut Input<'_>) -> PResult<bool> {
    ws(i)?;
    Ok(opt('(').parse_next(i)?.is_some())
}

/// After the blessed value: `, 'Class' )`.
pub(crate) fn bless_close<'a>(i: &mut Input<'a>, paren: bool) -> PResult<Cow<'a, str>> {
    ws(i)?;
    cut_err(','.context(StrContext::Expected(StrContextValue::CharLiteral(',')))).parse_next(i)?;
    ws(i)?;
    let Some(class) = opt(string_literal).parse_next(i)? else {
        return fail("expected class name in bless");
    };
    ws(i)?;
    opt(',').parse_next(i)?;
    if paren {
        close(i, ')')?;
    }
    Ok(class)
}

fn bless<'a>(i: &mut Input<'a>) -> PResult<Scalar<'a>> {
    let paren = bless_open(i)?;
    let inner = value(i)?;
    let class = bless_close(i, paren)?;
    Ok(Scalar::Blessed(class, Box::new(inner)))
}

/// `do{ \(my $o = ` -- Data::Dumper wraps blessed scalars this way.
pub(crate) fn do_open(i: &mut Input<'_>) -> PResult<()> {
    (
        (ws, '{', ws, '\\', ws, '('),
        (ws, kw("my"), ws, '$', ident, ws, '='),
    )
        .void()
        .context(expected("do{\\(my $o = ...)}"))
        .parse_next(i)
}

/// `) }`
pub(crate) fn do_close(i: &mut Input<'_>) -> PResult<()> {
    (ws, ')', ws, '}').void().parse_next(i)
}

/// `$VAR1`, `$VAR1->{'a'}[0]`, `${$VAR1}` ...
pub(crate) fn backref<'a>(i: &mut Input<'a>) -> PResult<BackRef<'a>> {
    let deref = opt("${").parse_next(i)?.is_some();
    '$'.parse_next(i)?;
    let var = ident(i)?;
    if deref {
        close(i, '}')?;
    }
    let mut path = Vec::new();
    if deref {
        path.push(Step::Deref);
    }
    let rest: Vec<Step<'a>> = repeat(0.., step).parse_next(i)?;
    path.extend(rest);
    Ok(BackRef {
        var: Cow::borrowed(var),
        path,
    })
}

fn step<'a>(i: &mut Input<'a>) -> PResult<Step<'a>> {
    (ws, opt("->")).void().parse_next(i)?;
    match any.parse_next(i)? {
        '{' => {
            let k = key_token(i)?;
            close(i, '}')?;
            Ok(Step::Key(k))
        }
        '[' => {
            ws(i)?;
            let Num::Int(n) = number(i)? else {
                return fail("expected array index");
            };
            close(i, ']')?;
            match usize::try_from(n) {
                Ok(n) => Ok(Step::Index(n)),
                Err(_) => fail("negative index"),
            }
        }
        _ => backtrack(),
    }
}

/// Replace every [`Scalar::BackRef`] in `vars` with a copy of the value it
/// points at. A reference that can't be found, or one that leads back into
/// itself (a true cycle), is an error.
///
/// This is a separate step because copying can multiply the size of the data
/// and because cycles can't be represented; callers who don't need it never
/// pay for it.
pub fn resolve_backrefs<'a>(vars: Vec<Scalar<'a>>) -> Result<Vec<Scalar<'a>>> {
    let mut out = vars.clone();
    for v in out.iter_mut() {
        resolve(v, &vars, 0)?;
    }
    Ok(out)
}

fn resolve<'a>(v: &mut Scalar<'a>, vars: &[Scalar<'a>], depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::Parse("cyclic back-reference".into()));
    }
    match v {
        Scalar::BackRef(br) => {
            let mut target = lookup(vars, br)?.clone();
            resolve(&mut target, vars, depth + 1)?;
            *v = target;
        }
        Scalar::Reference(r) => match &mut **r {
            Reference::Hash(h) => {
                for x in h.0.values_mut() {
                    resolve(x, vars, depth)?;
                }
            }
            Reference::Array(a) => {
                for x in a.0.iter_mut() {
                    resolve(x, vars, depth)?;
                }
            }
            Reference::Scalar(s) => resolve(s, vars, depth)?,
        },
        Scalar::Blessed(_, inner) => resolve(inner, vars, depth)?,
        _ => {}
    }
    Ok(())
}

fn lookup<'v, 'a>(vars: &'v [Scalar<'a>], br: &BackRef<'_>) -> Result<&'v Scalar<'a>> {
    let bad = || Error::Parse(format!("cannot resolve back-reference ${}", br.var));
    let idx: usize = br
        .var
        .strip_prefix("VAR")
        .and_then(|n| n.parse::<usize>().ok())
        .filter(|&n| n >= 1)
        .ok_or_else(bad)?;
    let mut cur = vars.get(idx - 1).ok_or_else(bad)?;
    for st in &br.path {
        while let Scalar::Blessed(_, inner) = cur {
            cur = inner;
        }
        cur = match (st, cur) {
            (Step::Deref, Scalar::Reference(r)) => match &**r {
                Reference::Scalar(s) => s,
                _ => return Err(bad()),
            },
            (Step::Key(k), Scalar::Reference(r)) => match &**r {
                Reference::Hash(h) => h.0.get(&**k).ok_or_else(bad)?,
                _ => return Err(bad()),
            },
            (Step::Index(i), Scalar::Reference(r)) => match &**r {
                Reference::Array(a) => a.0.get(*i).ok_or_else(bad)?,
                _ => return Err(bad()),
            },
            _ => return Err(bad()),
        };
    }
    Ok(cur)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: &str) -> Scalar<'_> {
        Scalar::String(x.into())
    }

    fn hash<'a>(pairs: Vec<(&'a str, Scalar<'a>)>) -> Scalar<'a> {
        Scalar::Reference(Box::new(Reference::Hash(Box::new(Hash(
            pairs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        )))))
    }

    fn array(v: Vec<Scalar<'_>>) -> Scalar<'_> {
        Scalar::Reference(Box::new(Reference::Array(Box::new(Array(v)))))
    }

    #[test]
    fn literals() {
        assert_eq!(parse("undef").unwrap(), Scalar::Undefined);
        assert_eq!(parse("123").unwrap(), Scalar::Int(123));
        assert_eq!(parse("-3").unwrap(), Scalar::Int(-3));
        assert_eq!(parse("1_000").unwrap(), Scalar::Int(1000));
        assert_eq!(parse("0x1F").unwrap(), Scalar::Int(31));
        assert_eq!(parse("123.456").unwrap(), Scalar::Float(123.456));
        assert_eq!(parse("1e5").unwrap(), Scalar::Float(1e5));
        assert_eq!(parse("-1.5E-3").unwrap(), Scalar::Float(-1.5e-3));
        assert_eq!(parse("99999999999999999999").unwrap(), Scalar::Float(1e20));
    }

    #[test]
    fn strings() {
        assert_eq!(parse("''").unwrap(), s(""));
        assert_eq!(parse(r"'it\'s \\ \n'").unwrap(), s("it's \\ \\n"));
        assert_eq!(parse(r#""""#).unwrap(), s(""));
        assert_eq!(
            parse(r#""a\n\x{263a}\x41\1\033\$\@\"\\""#).unwrap(),
            s("a\n\u{263a}A\u{1}\u{1b}$@\"\\")
        );
        assert_eq!(parse("q{a{b}c}").unwrap(), s("a{b}c"));
        assert_eq!(parse("q()").unwrap(), s(""));
        assert_eq!(parse(r"q!a\!b!").unwrap(), s("a!b"));
        assert_eq!(parse(r"qq(a\n)").unwrap(), s("a\n"));
        assert_eq!(parse("'caf\u{e9}'").unwrap(), s("caf\u{e9}"));
    }

    #[test]
    fn containers() {
        assert_eq!(
            parse("{ 'foo' => [ 'bar', { baz => 'qux' }, ], }").unwrap(),
            hash(vec![(
                "foo",
                array(vec![s("bar"), hash(vec![("baz", s("qux"))])])
            )])
        );
        assert_eq!(parse("{}").unwrap(), hash(vec![]));
        assert_eq!(parse("[]").unwrap(), array(vec![]));
        assert_eq!(
            parse("{1=>2, -3 => 4, 'a b'=>5, 'x', 6}").unwrap(),
            hash(vec![
                ("1", Scalar::Int(2)),
                ("-3", Scalar::Int(4)),
                ("a b", Scalar::Int(5)),
                ("x", Scalar::Int(6)),
            ])
        );
        assert_eq!(
            parse("\\\\'r'").unwrap(),
            Scalar::Reference(Box::new(Reference::Scalar(Box::new(Scalar::Reference(
                Box::new(Reference::Scalar(Box::new(s("r"))))
            )))))
        );
    }

    #[test]
    fn dumper_extras() {
        assert_eq!(
            parse("bless( { 'x' => 1 }, 'Foo::Bar' )").unwrap(),
            Scalar::Blessed(
                "Foo::Bar".into(),
                Box::new(hash(vec![("x", Scalar::Int(1))]))
            )
        );
        assert_eq!(
            parse("qr/a.b/i").unwrap(),
            Scalar::Regex {
                pattern: "a.b".into(),
                flags: "i".into()
            }
        );
        // code refs are deliberately unsupported
        assert!(parse(r#"sub { "DUMMY" }"#).is_err());
        assert_eq!(
            parse("\\*::STDOUT").unwrap(),
            Scalar::Reference(Box::new(Reference::Scalar(Box::new(Scalar::Glob))))
        );
    }

    #[test]
    fn var_assignment_and_backrefs() {
        let v = parse_all("$VAR1 = { 'a' => [1], 'b' => $VAR1->{'a'} };\n$VAR2 = 'x';\n").unwrap();
        assert_eq!(v.len(), 2);
        let backref = Scalar::BackRef(BackRef {
            var: "VAR1".into(),
            path: vec![Step::Key("a".into())],
        });
        assert_eq!(
            v[0],
            hash(vec![("a", array(vec![Scalar::Int(1)])), ("b", backref)])
        );
        assert_eq!(v[1], s("x"));

        let v = resolve_backrefs(v).unwrap();
        assert_eq!(
            v[0],
            hash(vec![
                ("a", array(vec![Scalar::Int(1)])),
                ("b", array(vec![Scalar::Int(1)]))
            ])
        );
        assert_eq!(parse("my $x = 5;").unwrap(), Scalar::Int(5));
    }

    #[test]
    fn backref_paths() {
        assert_eq!(
            parse("$VAR1 = $VAR2->{'a'}[0];").unwrap(),
            Scalar::BackRef(BackRef {
                var: "VAR2".into(),
                path: vec![Step::Key("a".into()), Step::Index(0)]
            })
        );
        assert_eq!(
            parse("[ ${$VAR1}, $VAR1->[1]{b} ]").unwrap(),
            array(vec![
                Scalar::BackRef(BackRef {
                    var: "VAR1".into(),
                    path: vec![Step::Deref]
                }),
                Scalar::BackRef(BackRef {
                    var: "VAR1".into(),
                    path: vec![Step::Index(1), Step::Key("b".into())]
                }),
            ])
        );
    }

    #[test]
    fn cycles_and_errors() {
        let cyclic = parse_all("$VAR1 = { 'a' => $VAR1 };").unwrap();
        assert!(resolve_backrefs(cyclic).is_err());
        let missing = parse_all("$VAR1 = $VAR9;").unwrap();
        assert!(resolve_backrefs(missing).is_err());
        assert!(parse("[1, 2").is_err());
        assert!(parse("[1] junk").is_err());
        assert!(parse("").is_err());
        assert!(parse(&"[".repeat(1000)).is_err());
    }

    #[test]
    fn numbers() {
        assert_eq!(parse("007").unwrap(), Scalar::Int(7));
        assert_eq!(parse("1_000.5").unwrap(), Scalar::Float(1000.5));
        assert_eq!(parse("-0x_ff").unwrap(), Scalar::Int(-255));
        assert_eq!(parse("0b101").unwrap(), Scalar::Int(5));
        assert_eq!(parse("+12").unwrap(), Scalar::Int(12));
        assert_eq!(
            parse("123456789012345678").unwrap(),
            Scalar::Int(123456789012345678)
        );
        assert_eq!(parse("9223372036854775807").unwrap(), Scalar::Int(i64::MAX));
        assert_eq!(
            parse("-9223372036854775808").unwrap(),
            Scalar::Int(i64::MIN)
        );
        assert_eq!(parse(".5").unwrap(), Scalar::Float(0.5));
        assert!(parse("-").is_err());
        assert!(parse("1e").is_err());
        assert_eq!(
            parse("[1e5,2]").unwrap(),
            array(vec![Scalar::Float(1e5), Scalar::Int(2)])
        );
    }

    #[test]
    fn borrowing() {
        let text = r#"{ plain => 'abc', esc => 'a\'b', dq => "x\ny", q => q{a{b}c} }"#;
        let Scalar::Reference(r) = parse(text).unwrap() else {
            panic!()
        };
        let Reference::Hash(h) = *r else { panic!() };
        let get = |k: &str| match &h.0[k] {
            Scalar::String(s) => s.is_borrowed(),
            _ => panic!(),
        };
        assert!(get("plain"));
        assert!(!get("esc"));
        assert!(!get("dq"));
        assert!(!get("q"));
        assert!(h.0.keys().all(|k| k.is_borrowed()));
    }

    #[test]
    fn assignments_keep_names() {
        let src = "# header\n$a = 1;\n\n# note\nmy $b = 'two'; $c = [3]; # trailing\n";
        let got = parse_assignments(src).unwrap();
        let names: Vec<&str> = got.iter().map(|(n, _)| &**n).collect();
        assert_eq!(names, ["a", "b", "c"]);
        assert_eq!(got[1].1, s("two"));
        assert!(parse_assignments("").unwrap().is_empty());
        assert!(parse_assignments("# only a comment\n").unwrap().is_empty());
        assert!(parse_assignments("$a = 1 $b = 2;").is_err());
        assert!(parse_assignments("1;").is_err());
        assert!(parse_assignments("$a = ;").is_err());
    }

    #[test]
    fn comments_and_whitespace() {
        assert_eq!(
            parse("  # hi\n [ 1 , # two\n 2 ]  ").unwrap(),
            array(vec![Scalar::Int(1), Scalar::Int(2)])
        );
    }
}
