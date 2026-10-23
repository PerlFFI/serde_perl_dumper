//! A deserializer that reads Perl source directly, without building a
//! [`Scalar`](crate::Scalar) tree first. Strings are borrowed from the input
//! when they need no unescaping.

use std::borrow::Cow as StdCow;
use std::fmt;

use beef::lean::Cow;
use serde::de::{
    self,
    value::{BorrowedStrDeserializer, StringDeserializer},
    Deserialize, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor,
};

use super::KeyDeserializer;
use crate::error::{Error, Result};
use crate::parser::{
    assign_head, at_assignment, backref, bless_close, bless_open, close, comma, do_close, do_open,
    error_at, fail, glob, key, kw, number, regex_body, string_literal, ws, Input, Num, PResult,
    State, MAX_DEPTH,
};
use winnow::combinator::opt;
use winnow::prelude::*;

/// Parse Perl source (a bare literal or Data::Dumper's `$VAR1 = ...;` output)
/// and deserialize it.
///
/// Only the first `$VARn` is deserialized; later statements are not
/// examined. Back-references (`$VAR1->{'a'}`) are an error here: use
/// [`parse_all`](crate::parse_all) and
/// [`resolve_backrefs`](crate::resolve_backrefs) for dumps that contain them.
pub fn from_str<'de, T>(src: &'de str) -> Result<T>
where
    T: Deserialize<'de>,
{
    let mut de = StrDeserializer::new(src);
    let assignment = de.p(|i| {
        ws(i)?;
        if i.input.is_empty() {
            return fail("empty input");
        }
        let a = at_assignment(i)?;
        if a {
            assign_head(i)?;
        }
        Ok(a)
    })?;
    let value = T::deserialize(&mut de)?;
    de.p(|i| {
        ws(i)?;
        if assignment {
            if opt(';').parse_next(i)?.is_none() && !i.input.is_empty() {
                return fail("expected ';'");
            }
            ws(i)?;
            if !i.input.is_empty() && !at_assignment(i)? {
                return fail("unexpected trailing input");
            }
        } else if !i.input.is_empty() {
            return fail("unexpected trailing input");
        }
        Ok(())
    })?;
    Ok(value)
}

struct StrDeserializer<'de> {
    src: &'de str,
    i: Input<'de>,
}

/// Something to close after the value it wraps.
enum Closer {
    Bless { paren: bool },
    Do,
}

impl<'de> StrDeserializer<'de> {
    fn new(src: &'de str) -> Self {
        StrDeserializer {
            src,
            i: Input {
                input: src,
                state: State::default(),
            },
        }
    }

    /// Run a winnow-level parser, turning its error into a positioned [`Error`].
    fn p<T>(&mut self, f: impl FnOnce(&mut Input<'de>) -> PResult<T>) -> Result<T> {
        let r = f(&mut self.i);
        r.map_err(|e| error_at(self.src, self.src.len() - self.i.input.len(), e))
    }

    fn err<T>(&mut self, msg: &'static str) -> Result<T> {
        self.p(|_| fail(msg))
    }

    /// The next byte after whitespace.
    fn peek(&mut self) -> Result<Option<u8>> {
        self.p(ws)?;
        Ok(self.i.input.as_bytes().first().copied())
    }

    fn at_keyword(&mut self, word: &'static str) -> Result<bool> {
        self.p(|i| Ok(opt(kw(word)).parse_next(i)?.is_some()))
    }

    /// Skip `\`, `bless(` and `do{\(my $o =` prefixes, which don't change what
    /// the value deserializes as.
    fn strip(&mut self, closers: &mut Vec<Closer>) -> Result<()> {
        loop {
            match self.peek()? {
                Some(b'\\') => self.i.input = &self.i.input[1..],
                Some(b'b') if self.at_keyword("bless")? => {
                    let paren = self.p(bless_open)?;
                    closers.push(Closer::Bless { paren });
                }
                Some(b'd') if self.at_keyword("do")? => {
                    self.p(do_open)?;
                    closers.push(Closer::Do);
                }
                _ => return Ok(()),
            }
        }
    }

    fn wrapped<R>(&mut self, f: impl FnOnce(&mut Self) -> Result<R>) -> Result<R> {
        self.i.state.depth += 1;
        let r = if self.i.state.depth > MAX_DEPTH {
            self.err("nesting too deep")
        } else {
            self.wrapped_inner(f)
        };
        self.i.state.depth -= 1;
        r
    }

    fn wrapped_inner<R>(&mut self, f: impl FnOnce(&mut Self) -> Result<R>) -> Result<R> {
        let mut closers = Vec::new();
        self.strip(&mut closers)?;
        let r = f(self)?;
        for c in closers.into_iter().rev() {
            match c {
                Closer::Bless { paren } => self.p(|i| bless_close(i, paren).map(|_| ()))?,
                Closer::Do => self.p(do_close)?,
            }
        }
        Ok(r)
    }

    fn is_string_start(&mut self) -> Result<bool> {
        Ok(match self.peek()? {
            Some(b'\'' | b'"') => true,
            Some(b'q') => {
                let rest = &self.i.input[1..];
                // q or qq followed by a delimiter
                let rest = rest.strip_prefix('q').unwrap_or(rest);
                rest.trim_start()
                    .starts_with(|c: char| c.is_ascii_punctuation() && c != '_')
            }
            _ => false,
        })
    }

    /// Deserialize without handling wrappers; callers have already stripped them.
    fn any<V: Visitor<'de>>(&mut self, visitor: V) -> Result<V::Value> {
        let Some(c) = self.peek()? else {
            return self.err("unexpected end of input");
        };
        match c {
            b'{' => {
                self.i.input = &self.i.input[1..];
                let mut acc = Access {
                    de: self,
                    done: false,
                    need_close: false,
                };
                let v = visitor.visit_map(&mut acc)?;
                acc.finish(b'}', "trailing entries in hash")?;
                Ok(v)
            }
            b'[' => {
                self.i.input = &self.i.input[1..];
                let mut acc = Access {
                    de: self,
                    done: false,
                    need_close: false,
                };
                let v = visitor.visit_seq(&mut acc)?;
                acc.finish(b']', "trailing elements in array")?;
                Ok(v)
            }
            b'\'' | b'"' => self.string_value(visitor),
            b'-' | b'+' | b'.' | b'0'..=b'9' => match self.p(number)? {
                Num::Int(n) => visitor.visit_i64(n),
                Num::Float(f) => visitor.visit_f64(f),
            },
            b'$' => self.err("unresolved back-reference; use parse_all and resolve_backrefs"),
            b'*' => self.err("cannot deserialize a glob"),
            _ => {
                if self.at_keyword("undef")? {
                    visitor.visit_unit()
                } else if self.at_keyword("qr")? {
                    let (pattern, _) = self.p(regex_body)?;
                    visit_cow(pattern, visitor)
                } else if self.is_string_start()? {
                    self.string_value(visitor)
                } else {
                    self.err("unexpected character")
                }
            }
        }
    }

    fn string_value<V: Visitor<'de>>(&mut self, visitor: V) -> Result<V::Value> {
        let s = self.p(string_literal)?;
        visit_cow(s, visitor)
    }

    /// A string, accepting numbers too (rendered as text).
    fn string(&mut self) -> Result<Cow<'de, str>> {
        if matches!(self.peek()?, Some(b'-' | b'+' | b'.' | b'0'..=b'9')) {
            return Ok(match self.p(number)? {
                Num::Int(n) => n.to_string().into(),
                Num::Float(f) => f.to_string().into(),
            });
        }
        self.any(CowVisitor)
    }
}

fn visit_cow<'de, V: Visitor<'de>>(s: Cow<'de, str>, visitor: V) -> Result<V::Value> {
    if s.is_borrowed() {
        visitor.visit_borrowed_str(s.unwrap_borrowed())
    } else {
        visitor.visit_string(s.into_owned())
    }
}

/// Reads a hash or array body. `need_close` is set when an element wasn't
/// followed by a comma, so the next thing must be the closing bracket.
struct Access<'a, 'de> {
    de: &'a mut StrDeserializer<'de>,
    done: bool,
    need_close: bool,
}

impl<'a, 'de> Access<'a, 'de> {
    /// Consume the closing bracket if the visitor didn't read up to it.
    fn finish(&mut self, bracket: u8, msg: &'static str) -> Result<()> {
        if self.done {
            return Ok(());
        }
        if self.de.peek()? == Some(bracket) {
            self.de.i.input = &self.de.i.input[1..];
            Ok(())
        } else {
            self.de.err(msg)
        }
    }

    /// Returns true (and marks done) at the closing bracket.
    fn at_end(&mut self, bracket: u8) -> Result<bool> {
        if self.done {
            return Ok(true);
        }
        if self.de.peek()? == Some(bracket) {
            self.de.i.input = &self.de.i.input[1..];
            self.done = true;
            return Ok(true);
        }
        if self.need_close {
            return self.de.err("expected ',' or closing bracket");
        }
        Ok(false)
    }

    fn after_element(&mut self) -> Result<()> {
        self.de.p(ws)?;
        let comma_seen = self.de.p(|i| Ok(opt(comma).parse_next(i)?.is_some()))?;
        self.need_close = !comma_seen;
        Ok(())
    }
}

impl<'a, 'de> SeqAccess<'de> for &mut Access<'a, 'de> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>> {
        if self.at_end(b']')? {
            return Ok(None);
        }
        let v = seed.deserialize(&mut *self.de)?;
        self.after_element()?;
        Ok(Some(v))
    }
}

impl<'a, 'de> MapAccess<'de> for &mut Access<'a, 'de> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>> {
        if self.at_end(b'}')? {
            return Ok(None);
        }
        let k = self.de.p(key)?;
        seed.deserialize(KeyDeserializer(k)).map(Some)
    }

    fn next_value_seed<S: DeserializeSeed<'de>>(&mut self, seed: S) -> Result<S::Value> {
        let v = seed.deserialize(&mut *self.de)?;
        self.after_element()?;
        Ok(v)
    }
}

/// Collects a string (borrowed if possible) from `any`.
struct CowVisitor;

impl<'de> Visitor<'de> for CowVisitor {
    type Value = Cow<'de, str>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a string")
    }

    fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(Cow::borrowed(v))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Self::Value, E> {
        Ok(v.to_owned().into())
    }

    fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Self::Value, E> {
        Ok(v.into())
    }
}

/// A number parsed from a number token or a numeric string.
enum NumVal {
    I(i64),
    U(u64),
    F(f64),
}

struct NumVisitor;

impl<'de> Visitor<'de> for NumVisitor {
    type Value = NumVal;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a number")
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<NumVal, E> {
        Ok(NumVal::I(v))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<NumVal, E> {
        Ok(NumVal::U(v))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<NumVal, E> {
        Ok(NumVal::F(v))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<NumVal, E> {
        let v = v.trim();
        if let Ok(n) = v.parse::<i64>() {
            Ok(NumVal::I(n))
        } else if let Ok(n) = v.parse::<u64>() {
            Ok(NumVal::U(n))
        } else if let Ok(f) = v.parse::<f64>() {
            Ok(NumVal::F(f))
        } else {
            Err(E::custom(format!("invalid number {v:?}")))
        }
    }
}

impl NumVal {
    fn to_i64(&self) -> Result<i64> {
        match *self {
            NumVal::I(n) => Ok(n),
            NumVal::U(n) => i64::try_from(n).map_err(|_| de::Error::custom("integer out of range")),
            NumVal::F(f) if f.fract() == 0.0 && f.abs() < 9.2e18 => Ok(f as i64),
            NumVal::F(_) => Err(de::Error::custom("expected integer, found float")),
        }
    }

    fn to_u64(&self) -> Result<u64> {
        match *self {
            NumVal::U(n) => Ok(n),
            NumVal::F(f) if f.fract() == 0.0 && (0.0..1.8e19).contains(&f) => Ok(f as u64),
            _ => u64::try_from(self.to_i64()?)
                .map_err(|_| de::Error::custom("negative value for unsigned integer")),
        }
    }

    fn to_f64(&self) -> f64 {
        match *self {
            NumVal::I(n) => n as f64,
            NumVal::U(n) => n as f64,
            NumVal::F(f) => f,
        }
    }
}

/// Perl truthiness: undef, "", "0" and 0 are false.
struct TruthVisitor;

impl<'de> Visitor<'de> for TruthVisitor {
    type Value = bool;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a value")
    }

    fn visit_unit<E: de::Error>(self) -> std::result::Result<bool, E> {
        Ok(false)
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<bool, E> {
        Ok(v != 0)
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<bool, E> {
        Ok(v != 0)
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<bool, E> {
        Ok(v != 0.0)
    }

    fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<bool, E> {
        Ok(!(v.is_empty() || v == "0"))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> std::result::Result<bool, A::Error> {
        while a.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(true)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> std::result::Result<bool, A::Error> {
        while a.next_element::<IgnoredAny>()?.is_some() {}
        Ok(true)
    }
}

macro_rules! de_int {
    ($($method:ident $visit:ident $get:ident $ty:ty;)*) => {$(
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
            let n = self.wrapped(|d| d.any(NumVisitor))?;
            let v = <$ty>::try_from(n.$get()?)
                .map_err(|_| de::Error::custom(concat!("value out of range for ", stringify!($ty))))?;
            visitor.$visit(v)
        }
    )*};
}

impl<'de> de::Deserializer<'de> for &mut StrDeserializer<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.wrapped(|d| d.any(visitor))
    }

    de_int! {
        deserialize_i8 visit_i8 to_i64 i8;
        deserialize_i16 visit_i16 to_i64 i16;
        deserialize_i32 visit_i32 to_i64 i32;
        deserialize_i64 visit_i64 to_i64 i64;
        deserialize_u8 visit_u8 to_u64 u8;
        deserialize_u16 visit_u16 to_u64 u16;
        deserialize_u32 visit_u32 to_u64 u32;
        deserialize_u64 visit_u64 to_u64 u64;
    }

    fn deserialize_i128<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let n = self.wrapped(|d| d.any(NumVisitor))?;
        visitor.visit_i128(i128::from(n.to_i64()?))
    }

    fn deserialize_u128<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let n = self.wrapped(|d| d.any(NumVisitor))?;
        visitor.visit_u128(u128::from(n.to_u64()?))
    }

    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let n = self.wrapped(|d| d.any(NumVisitor))?;
        visitor.visit_f32(n.to_f64() as f32)
    }

    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let n = self.wrapped(|d| d.any(NumVisitor))?;
        visitor.visit_f64(n.to_f64())
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let b = self.wrapped(|d| d.any(TruthVisitor))?;
        visitor.visit_bool(b)
    }

    fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let s = self.wrapped(|d| d.string())?;
        let mut it = s.chars();
        match (it.next(), it.next()) {
            (Some(c), None) => visitor.visit_char(c),
            _ => Err(de::Error::custom("expected a single character")),
        }
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let s = self.wrapped(|d| d.string())?;
        visit_cow(s, visitor)
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_str(visitor)
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.wrapped(|d| {
            if d.is_string_start()? {
                let s = d.p(string_literal)?;
                match StdCow::<str>::from(s) {
                    StdCow::Borrowed(b) => visitor.visit_borrowed_bytes(b.as_bytes()),
                    StdCow::Owned(o) => visitor.visit_byte_buf(o.into_bytes()),
                }
            } else {
                d.any(visitor)
            }
        })
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_bytes(visitor)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.wrapped(|d| {
            if d.peek()?.is_some() && d.at_keyword("undef")? {
                visitor.visit_none()
            } else {
                visitor.visit_some(&mut *d)
            }
        })
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.wrapped(|d| {
            if d.at_keyword("undef")? {
                visitor.visit_unit()
            } else {
                d.err("expected undef")
            }
        })
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value> {
        self.deserialize_unit(visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        self.wrapped(|d| match d.peek()? {
            Some(b'{') => {
                d.i.input = &d.i.input[1..];
                let variant = d.p(key)?;
                visitor.visit_enum(EnumAccess { de: d, variant })
            }
            _ if d.is_string_start()? => {
                let s = d.p(string_literal)?;
                if s.is_borrowed() {
                    visitor.visit_enum(BorrowedStrDeserializer::<Error>::new(s.unwrap_borrowed()))
                } else {
                    visitor.visit_enum(StringDeserializer::<Error>::new(s.into_owned()))
                }
            }
            _ => d.err("expected a string or a hash with one key for enum"),
        })
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.wrapped(|d| match d.peek()? {
            Some(b'*') => {
                d.p(glob)?;
                visitor.visit_unit()
            }
            Some(b'$') => {
                d.p(|i| backref(i).map(|_| ()))?;
                visitor.visit_unit()
            }
            _ => d.any(visitor),
        })
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_any(visitor)
    }

    fn deserialize_tuple<V: Visitor<'de>>(self, _len: usize, visitor: V) -> Result<V::Value> {
        self.deserialize_any(visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value> {
        self.deserialize_any(visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_any(visitor)
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        self.deserialize_any(visitor)
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_any(visitor)
    }

    fn is_human_readable(&self) -> bool {
        true
    }
}

struct EnumAccess<'a, 'de> {
    de: &'a mut StrDeserializer<'de>,
    variant: Cow<'de, str>,
}

impl<'a, 'de> de::EnumAccess<'de> for EnumAccess<'a, 'de> {
    type Error = Error;
    type Variant = Self;

    fn variant_seed<V: DeserializeSeed<'de>>(mut self, seed: V) -> Result<(V::Value, Self)> {
        let variant = std::mem::take(&mut self.variant);
        let v = seed.deserialize(KeyDeserializer(variant))?;
        Ok((v, self))
    }
}

impl<'a, 'de> EnumAccess<'a, 'de> {
    /// After the variant's value: an optional comma, then `}`.
    fn end(&mut self) -> Result<()> {
        self.de.p(|i| {
            ws(i)?;
            opt(comma).parse_next(i)?;
            close(i, '}')
        })
    }
}

impl<'a, 'de> de::VariantAccess<'de> for EnumAccess<'a, 'de> {
    type Error = Error;

    fn unit_variant(mut self) -> Result<()> {
        <()>::deserialize(&mut *self.de)?;
        self.end()
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(mut self, seed: T) -> Result<T::Value> {
        let v = seed.deserialize(&mut *self.de)?;
        self.end()?;
        Ok(v)
    }

    fn tuple_variant<V: Visitor<'de>>(mut self, _len: usize, visitor: V) -> Result<V::Value> {
        let v = de::Deserializer::deserialize_any(&mut *self.de, visitor)?;
        self.end()?;
        Ok(v)
    }

    fn struct_variant<V: Visitor<'de>>(
        mut self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        let v = de::Deserializer::deserialize_any(&mut *self.de, visitor)?;
        self.end()?;
        Ok(v)
    }
}
