use beef::lean::Cow;
use serde::{
    de::{self, DeserializeSeed, IntoDeserializer, MapAccess, SeqAccess, Visitor},
    forward_to_deserialize_any, Deserialize,
};

mod stream;
pub use stream::from_str;

use crate::error::{Error, Result};
use crate::parser::{self, Reference, Scalar};

pub struct Deserializer<'de> {
    scalar: &'de Scalar<'de>,
}

impl<'de> Deserializer<'de> {
    fn new(scalar: &'de Scalar<'de>) -> Self {
        Deserializer { scalar }
    }

    /// The scalar with references and blessings stripped away.
    fn target(&self) -> &'de Scalar<'de> {
        let mut cur = self.scalar;
        loop {
            match cur {
                Scalar::Blessed(_, inner) => cur = inner,
                Scalar::Reference(r) => match &**r {
                    Reference::Scalar(inner) => cur = inner,
                    _ => return cur,
                },
                _ => return cur,
            }
        }
    }

    fn as_i64(&self) -> Result<i64> {
        match self.target() {
            Scalar::Int(i) => Ok(*i),
            Scalar::Float(f) if f.fract() == 0.0 && f.abs() < 9.2e18 => Ok(*f as i64),
            Scalar::String(s) => s
                .trim()
                .parse::<i64>()
                .or_else(|_| {
                    s.trim()
                        .parse::<f64>()
                        .ok()
                        .filter(|f| f.fract() == 0.0 && f.abs() < 9.2e18)
                        .map(|f| f as i64)
                        .ok_or(())
                })
                .map_err(|_| de::Error::custom(format!("invalid integer {s:?}"))),
            other => Err(de::Error::custom(format!(
                "expected integer, found {}",
                kind(other)
            ))),
        }
    }

    fn as_u64(&self) -> Result<u64> {
        if let Scalar::String(s) = self.target() {
            if let Ok(u) = s.trim().parse::<u64>() {
                return Ok(u);
            }
        }
        u64::try_from(self.as_i64()?)
            .map_err(|_| de::Error::custom("negative value for unsigned integer"))
    }

    fn as_f64(&self) -> Result<f64> {
        match self.target() {
            Scalar::Int(i) => Ok(*i as f64),
            Scalar::Float(f) => Ok(*f),
            Scalar::String(s) => s
                .trim()
                .parse::<f64>()
                .map_err(|_| de::Error::custom(format!("invalid number {s:?}"))),
            other => Err(de::Error::custom(format!(
                "expected number, found {}",
                kind(other)
            ))),
        }
    }
}

fn kind(s: &Scalar<'_>) -> &'static str {
    match s {
        Scalar::Undefined => "undef",
        Scalar::Int(_) => "integer",
        Scalar::Float(_) => "float",
        Scalar::String(_) => "string",
        Scalar::Reference(r) => match **r {
            Reference::Hash(_) => "hash",
            Reference::Array(_) => "array",
            Reference::Scalar(_) => "scalar reference",
        },
        Scalar::Blessed(..) => "blessed object",
        Scalar::Regex { .. } => "regex",
        Scalar::Glob => "glob",
        Scalar::BackRef(_) => "unresolved back-reference",
    }
}

/// Deserialize from an already-parsed [`Scalar`].
pub fn from_perl<'de, T>(scalar: &'de Scalar<'_>) -> Result<T>
where
    T: Deserialize<'de>,
{
    T::deserialize(Deserializer::new(scalar))
}

/// Deserialize a file of `$name = value;` statements (a typical Perl config
/// file) into a struct or map keyed by variable name, without the sigil. Variables the target type doesn't know about are ignored. As in
/// Perl, the last assignment to a name wins.
///
/// Unlike [`from_str`] this builds a tree first, so it requires an owned
/// target type.
pub fn from_assignments<T>(src: &str) -> Result<T>
where
    T: de::DeserializeOwned,
{
    let map = parser::parse_assignments(src)?.into_iter().collect();
    let scalar = Scalar::Reference(Box::new(Reference::Hash(Box::new(parser::Hash(map)))));
    T::deserialize(Deserializer::new(&scalar))
}

macro_rules! de_int {
    ($($method:ident $visit:ident $get:ident $ty:ty;)*) => {$(
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
            let v = <$ty>::try_from(self.$get()?)
                .map_err(|_| de::Error::custom(concat!("value out of range for ", stringify!($ty))))?;
            visitor.$visit(v)
        }
    )*};
}

impl<'de> de::Deserializer<'de> for Deserializer<'de> {
    type Error = Error;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.target() {
            Scalar::Undefined => visitor.visit_unit(),
            Scalar::Int(i) => visitor.visit_i64(*i),
            Scalar::Float(f) => visitor.visit_f64(*f),
            Scalar::String(s) => visitor.visit_borrowed_str(s),
            Scalar::Regex { pattern, .. } => visitor.visit_borrowed_str(pattern),
            Scalar::Reference(r) => match &**r {
                Reference::Hash(h) => visitor.visit_map(HashAccess::new(h.0.iter())),
                Reference::Array(a) => visitor.visit_seq(ArrayAccess { iter: a.0.iter() }),
                Reference::Scalar(_) => unreachable!("target() strips scalar references"),
            },
            other => Err(de::Error::custom(format!(
                "cannot deserialize {}",
                kind(other)
            ))),
        }
    }

    de_int! {
        deserialize_i8 visit_i8 as_i64 i8;
        deserialize_i16 visit_i16 as_i64 i16;
        deserialize_i32 visit_i32 as_i64 i32;
        deserialize_i64 visit_i64 as_i64 i64;
        deserialize_u8 visit_u8 as_u64 u8;
        deserialize_u16 visit_u16 as_u64 u16;
        deserialize_u32 visit_u32 as_u64 u32;
        deserialize_u64 visit_u64 as_u64 u64;
    }

    fn deserialize_i128<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_i128(i128::from(self.as_i64()?))
    }

    fn deserialize_u128<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_u128(u128::from(self.as_u64()?))
    }

    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_f32(self.as_f64()? as f32)
    }

    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_f64(self.as_f64()?)
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        // Perl truthiness: undef, "", "0" and 0 are false.
        let b = match self.target() {
            Scalar::Undefined => false,
            Scalar::Int(i) => *i != 0,
            Scalar::Float(f) => *f != 0.0,
            Scalar::String(s) => !(s.is_empty() || s == "0"),
            _ => true,
        };
        visitor.visit_bool(b)
    }

    fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.target() {
            Scalar::String(s) => {
                let mut it = s.chars();
                match (it.next(), it.next()) {
                    (Some(c), None) => visitor.visit_char(c),
                    _ => Err(de::Error::custom("expected a single character")),
                }
            }
            other => Err(de::Error::custom(format!(
                "expected string, found {}",
                kind(other)
            ))),
        }
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.target() {
            Scalar::Int(i) => visitor.visit_string(i.to_string()),
            Scalar::Float(f) => visitor.visit_string(f.to_string()),
            _ => self.deserialize_any(visitor),
        }
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_str(visitor)
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.target() {
            Scalar::String(s) => visitor.visit_borrowed_bytes(s.as_bytes()),
            _ => self.deserialize_any(visitor),
        }
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_bytes(visitor)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.target() {
            Scalar::Undefined => visitor.visit_none(),
            _ => visitor.visit_some(self),
        }
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.target() {
            Scalar::Undefined => visitor.visit_unit(),
            other => Err(de::Error::custom(format!(
                "expected undef, found {}",
                kind(other)
            ))),
        }
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
        match self.target() {
            Scalar::String(s) => visitor.visit_enum((&**s).into_deserializer()),
            Scalar::Reference(r) => match &**r {
                Reference::Hash(h) if h.0.len() == 1 => {
                    let (variant, value) = h.0.iter().next().unwrap();
                    visitor.visit_enum(EnumAccess { variant, value })
                }
                _ => Err(de::Error::custom(
                    "expected a hash with exactly one key for enum",
                )),
            },
            other => Err(de::Error::custom(format!(
                "expected enum, found {}",
                kind(other)
            ))),
        }
    }

    forward_to_deserialize_any! {
        seq tuple tuple_struct map struct identifier
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_unit()
    }

    fn is_human_readable(&self) -> bool {
        true
    }
}

struct HashAccess<'de, I> {
    iter: I,
    value: Option<&'de Scalar<'de>>,
}

impl<'de, I> HashAccess<'de, I> {
    fn new(iter: I) -> Self {
        HashAccess { iter, value: None }
    }
}

impl<'de, I> MapAccess<'de> for HashAccess<'de, I>
where
    I: Iterator<Item = (&'de beef::lean::Cow<'de, str>, &'de Scalar<'de>)>,
{
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>> {
        match self.iter.next() {
            Some((k, v)) => {
                self.value = Some(v);
                seed.deserialize(KeyDeserializer(Cow::borrowed(&**k)))
                    .map(Some)
            }
            None => Ok(None),
        }
    }

    fn next_value_seed<S: DeserializeSeed<'de>>(&mut self, seed: S) -> Result<S::Value> {
        let v = self
            .value
            .take()
            .ok_or_else(|| de::Error::custom("value without key"))?;
        seed.deserialize(Deserializer::new(v))
    }
}

/// Hash keys are always strings in Perl; parse them on demand for numeric key types.
pub(super) struct KeyDeserializer<'de>(pub Cow<'de, str>);

macro_rules! key_num {
    ($($method:ident $visit:ident $ty:ty;)*) => {$(
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
            let v = self.0.trim().parse::<$ty>()
                .map_err(|_| de::Error::custom(format!("invalid key {:?} for {}", &*self.0, stringify!($ty))))?;
            visitor.$visit(v)
        }
    )*};
}

impl<'de> de::Deserializer<'de> for KeyDeserializer<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        if self.0.is_borrowed() {
            visitor.visit_borrowed_str(self.0.unwrap_borrowed())
        } else {
            visitor.visit_string(self.0.into_owned())
        }
    }

    key_num! {
        deserialize_i8 visit_i8 i8;
        deserialize_i16 visit_i16 i16;
        deserialize_i32 visit_i32 i32;
        deserialize_i64 visit_i64 i64;
        deserialize_i128 visit_i128 i128;
        deserialize_u8 visit_u8 u8;
        deserialize_u16 visit_u16 u16;
        deserialize_u32 visit_u32 u32;
        deserialize_u64 visit_u64 u64;
        deserialize_u128 visit_u128 u128;
        deserialize_f32 visit_f32 f32;
        deserialize_f64 visit_f64 f64;
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_some(self)
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
        if self.0.is_borrowed() {
            visitor.visit_enum(self.0.unwrap_borrowed().into_deserializer())
        } else {
            visitor.visit_enum(self.0.into_owned().into_deserializer())
        }
    }

    forward_to_deserialize_any! {
        bool char str string bytes byte_buf unit unit_struct seq tuple
        tuple_struct map struct identifier ignored_any
    }
}

struct ArrayAccess<'de> {
    iter: std::slice::Iter<'de, Scalar<'de>>,
}

impl<'de> SeqAccess<'de> for ArrayAccess<'de> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>> {
        match self.iter.next() {
            Some(v) => seed.deserialize(Deserializer::new(v)).map(Some),
            None => Ok(None),
        }
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.iter.len())
    }
}

struct EnumAccess<'de> {
    variant: &'de str,
    value: &'de Scalar<'de>,
}

impl<'de> de::EnumAccess<'de> for EnumAccess<'de> {
    type Error = Error;
    type Variant = Deserializer<'de>;

    fn variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, Self::Variant)> {
        let v = seed.deserialize(self.variant.into_deserializer())?;
        Ok((v, Deserializer::new(self.value)))
    }
}

impl<'de> de::VariantAccess<'de> for Deserializer<'de> {
    type Error = Error;

    fn unit_variant(self) -> Result<()> {
        de::Deserializer::deserialize_unit(self, de::IgnoredAny).map(|_| ())
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value> {
        seed.deserialize(self)
    }

    fn tuple_variant<V: Visitor<'de>>(self, _len: usize, visitor: V) -> Result<V::Value> {
        de::Deserializer::deserialize_any(self, visitor)
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        de::Deserializer::deserialize_any(self, visitor)
    }
}

impl<'de> IntoDeserializer<'de, Error> for &'de Scalar<'de> {
    type Deserializer = Deserializer<'de>;

    fn into_deserializer(self) -> Self::Deserializer {
        Deserializer::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::collections::BTreeMap;

    #[derive(Deserialize, Debug, PartialEq)]
    struct Test {
        a: i32,
        b: String,
        topic: String,
        c: Vec<i32>,
    }

    #[test]
    fn test_deserialize_perl() {
        let t: Test =
            from_str(r#"{a => 42, 'b' => 'hello', "topic" => "\nworld", 'c' => [1, 2, 3]}"#)
                .unwrap();
        assert_eq!(
            t,
            Test {
                a: 42,
                b: "hello".into(),
                topic: "\nworld".into(),
                c: vec![1, 2, 3]
            }
        );
    }

    #[test]
    fn test_dumper_output() {
        let t: Test = from_str(
            "$VAR1 = bless( {\n 'a' => '42',\n 'b' => 'x',\n 'topic' => '',\n 'c' => [ 1, '2' ]\n }, 'Foo' );\n",
        )
        .unwrap();
        assert_eq!(
            t,
            Test {
                a: 42,
                b: "x".into(),
                topic: "".into(),
                c: vec![1, 2]
            }
        );
    }

    #[test]
    fn test_options_and_defaults() {
        #[derive(Deserialize, Debug, PartialEq)]
        struct S {
            a: Option<i64>,
            b: Option<String>,
            c: bool,
            d: f64,
            e: Option<Vec<u8>>,
        }
        let s: S = from_str("{a => 5, b => undef, c => '', d => '1.5', e => [1,2]}").unwrap();
        assert_eq!(
            s,
            S {
                a: Some(5),
                b: None,
                c: false,
                d: 1.5,
                e: Some(vec![1, 2])
            }
        );
        assert!(from_str::<u8>("300").is_err());
        assert!(from_str::<u32>("-1").is_err());
    }

    #[test]
    fn test_enums() {
        #[derive(Deserialize, Debug, PartialEq)]
        enum E {
            Unit,
            New(i32),
            Tup(i32, String),
            St { x: i32 },
        }
        assert_eq!(from_str::<E>("'Unit'").unwrap(), E::Unit);
        assert_eq!(from_str::<E>("{New => 3}").unwrap(), E::New(3));
        assert_eq!(
            from_str::<E>("{Tup => [1, 'a']}").unwrap(),
            E::Tup(1, "a".into())
        );
        assert_eq!(from_str::<E>("{St => {x => 1}}").unwrap(), E::St { x: 1 });
        assert!(from_str::<E>("'Nope'").is_err());
    }

    #[test]
    fn test_maps_refs_tuples() {
        let m: BTreeMap<String, Vec<(i32, String)>> =
            from_str("{ k => [[1, 'a'], [2, 'b']] }").unwrap();
        assert_eq!(m["k"], vec![(1, "a".to_string()), (2, "b".to_string())]);
        assert_eq!(from_str::<String>("\\\\'deref'").unwrap(), "deref");
        let m: BTreeMap<i32, i32> = from_str("{1 => 2, 3 => 4}").unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(from_str::<()>("undef").unwrap(), ());
        assert_eq!(from_str::<char>("'x'").unwrap(), 'x');
    }

    #[test]
    fn test_borrowed() {
        let scalar = crate::parse("{ a => 'hi' }").unwrap();
        #[derive(Deserialize)]
        struct B<'a> {
            a: &'a str,
        }
        let b: B = from_perl(&scalar).unwrap();
        assert_eq!(b.a, "hi");
    }

    /// The streaming `from_str` and the tree-based `parse` + `from_perl` must agree.
    #[test]
    fn streaming_matches_tree() {
        #[derive(Debug, PartialEq)]
        enum V {
            Unit,
            I(i64),
            F(f64),
            S(String),
            Seq(Vec<V>),
            Map(BTreeMap<String, V>),
        }
        impl<'de> Deserialize<'de> for V {
            fn deserialize<D: de::Deserializer<'de>>(d: D) -> std::result::Result<V, D::Error> {
                struct Vis;
                impl<'de> Visitor<'de> for Vis {
                    type Value = V;
                    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        f.write_str("anything")
                    }
                    fn visit_unit<E>(self) -> std::result::Result<V, E> {
                        Ok(V::Unit)
                    }
                    fn visit_i64<E>(self, v: i64) -> std::result::Result<V, E> {
                        Ok(V::I(v))
                    }
                    fn visit_f64<E>(self, v: f64) -> std::result::Result<V, E> {
                        Ok(V::F(v))
                    }
                    fn visit_str<E>(self, v: &str) -> std::result::Result<V, E> {
                        Ok(V::S(v.to_string()))
                    }
                    fn visit_seq<A: SeqAccess<'de>>(
                        self,
                        mut a: A,
                    ) -> std::result::Result<V, A::Error> {
                        let mut v = Vec::new();
                        while let Some(x) = a.next_element()? {
                            v.push(x);
                        }
                        Ok(V::Seq(v))
                    }
                    fn visit_map<A: MapAccess<'de>>(
                        self,
                        mut a: A,
                    ) -> std::result::Result<V, A::Error> {
                        let mut m = BTreeMap::new();
                        while let Some((k, v)) = a.next_entry()? {
                            m.insert(k, v);
                        }
                        Ok(V::Map(m))
                    }
                }
                d.deserialize_any(Vis)
            }
        }

        let inputs = [
            "undef",
            "42",
            "-1.5e3",
            "'it\\'s'",
            r#""a\n\x{263a}""#,
            "q{a{b}c}",
            "[1, 'two', [3, [4]], {a => undef}]",
            "{ 1 => 2, 'x y' => [], z => {} }",
            "bless( { 'a' => bless( [1], 'In' ) }, 'Out' )",
            "\\\\'deep'",
            "\\[1,2]",
            "do{\\(my $o = 5)}",
            "bless( do{\\(my $o = 'x')}, 'S' )",
            "qr/a\\.b/i",
            "$VAR1 = { 'a' => [ 1, 2, ], };\n",
            "# comment\n [ 1, # two\n 2 ]",
        ];
        for input in inputs {
            let streamed: V = from_str(input).unwrap_or_else(|e| panic!("{input}: {e}"));
            let tree = crate::parse(input).unwrap();
            let via_tree: V = from_perl(&tree).unwrap_or_else(|e| panic!("{input}: {e}"));
            assert_eq!(streamed, via_tree, "{input}");
        }
        for bad in [
            "[1, 2",
            "{a => 1 b => 2}",
            "[1,,2]",
            "'abc",
            "[1] x",
            "{a}",
            "$VAR1 = [1] $VAR2 = 2;",
        ] {
            assert!(from_str::<V>(bad).is_err(), "{bad}");
            assert!(crate::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn streaming_borrows_and_backrefs() {
        #[derive(Deserialize)]
        struct B<'a> {
            a: &'a str,
            c: Option<&'a str>,
        }
        let text = String::from("$VAR1 = { 'a' => 'plain', 'c' => undef };");
        let b: B = from_str(&text).unwrap();
        assert_eq!((b.a, b.c), ("plain", None));
        // &str can't hold an unescaped string
        assert!(from_str::<B>(r"{ a => 'es\'c' }").is_err());

        let err = from_str::<Vec<i32>>("$VAR1 = [1, $VAR1->[0]];").unwrap_err();
        assert!(err.to_string().contains("back-reference"), "{err}");
        // ...but ignored fields that hold them are fine
        #[derive(Deserialize)]
        struct One {
            a: i32,
        }
        assert_eq!(
            from_str::<One>("{ a => 1, b => $VAR1->{a}, c => \\*::STDOUT }")
                .unwrap()
                .a,
            1
        );
    }

    #[test]
    fn streaming_depth_limit() {
        assert!(from_str::<de::IgnoredAny>(&"[".repeat(10_000)).is_err());
        assert!(
            from_str::<de::IgnoredAny>(&format!("{}1{}", "[".repeat(100), "]".repeat(100))).is_ok()
        );
    }
}
