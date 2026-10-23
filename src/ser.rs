//! Serialization into a [`Value`] tree, which [`Dumper`](crate::Dumper) then
//! formats according to its options.

use serde::ser::{self, Impossible, Serialize};

use crate::dumper::Dumper;
use crate::error::{Error, Result};
use crate::quote::float_text;

/// Serialize to compact Perl source such as `{a=>1,b=>[1,2]}`: no whitespace,
/// no `$VAR1 = ` wrapper, bare keys where possible. Reads back with
/// [`from_str`](crate::from_str) and evaluates as plain Perl. For output that
/// looks like `Data::Dumper`'s, use [`Dumper`].
pub fn to_string<T>(value: &T) -> Result<String>
where
    T: Serialize + ?Sized,
{
    Dumper::builder()
        .indent(0)
        .terse(true)
        .quotekeys(false)
        .pair("=>")
        .lossless_floats(true)
        .build()
        .dump(value)
}

/// A serialized value: the Perl data model, minus references and objects.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Undef,
    Int(i64),
    UInt(u64),
    Float(f64),
    Str(String),
    Array(Vec<Value>),
    /// Entries in the order they were serialized.
    Hash(Vec<(String, Value)>),
}

/// Serialize anything into a [`Value`].
pub fn to_value<T>(value: &T) -> Result<Value>
where
    T: Serialize + ?Sized,
{
    value.serialize(ValueSerializer)
}

struct ValueSerializer;

impl ser::Serializer for ValueSerializer {
    type Ok = Value;
    type Error = Error;

    type SerializeSeq = SeqSerializer;
    type SerializeTuple = SeqSerializer;
    type SerializeTupleStruct = SeqSerializer;
    type SerializeTupleVariant = VariantSerializer<SeqSerializer>;
    type SerializeMap = MapSerializer;
    type SerializeStruct = MapSerializer;
    type SerializeStructVariant = VariantSerializer<MapSerializer>;

    fn serialize_bool(self, v: bool) -> Result<Value> {
        Ok(Value::Int(i64::from(v)))
    }

    fn serialize_i8(self, v: i8) -> Result<Value> {
        Ok(Value::Int(v.into()))
    }

    fn serialize_i16(self, v: i16) -> Result<Value> {
        Ok(Value::Int(v.into()))
    }

    fn serialize_i32(self, v: i32) -> Result<Value> {
        Ok(Value::Int(v.into()))
    }

    fn serialize_i64(self, v: i64) -> Result<Value> {
        Ok(Value::Int(v))
    }

    fn serialize_i128(self, v: i128) -> Result<Value> {
        Ok(match i64::try_from(v) {
            Ok(n) => Value::Int(n),
            Err(_) => Value::Str(v.to_string()),
        })
    }

    fn serialize_u8(self, v: u8) -> Result<Value> {
        Ok(Value::UInt(v.into()))
    }

    fn serialize_u16(self, v: u16) -> Result<Value> {
        Ok(Value::UInt(v.into()))
    }

    fn serialize_u32(self, v: u32) -> Result<Value> {
        Ok(Value::UInt(v.into()))
    }

    fn serialize_u64(self, v: u64) -> Result<Value> {
        Ok(Value::UInt(v))
    }

    fn serialize_u128(self, v: u128) -> Result<Value> {
        Ok(match u64::try_from(v) {
            Ok(n) => Value::UInt(n),
            Err(_) => Value::Str(v.to_string()),
        })
    }

    fn serialize_f32(self, v: f32) -> Result<Value> {
        // use f32's own shortest digits so 0.1f32 doesn't become 0.10000000149011612
        let mut buf = ryu::Buffer::new();
        Ok(Value::Float(buf.format(v).parse().unwrap_or(f64::from(v))))
    }

    fn serialize_f64(self, v: f64) -> Result<Value> {
        Ok(Value::Float(v))
    }

    fn serialize_char(self, v: char) -> Result<Value> {
        Ok(Value::Str(v.to_string()))
    }

    fn serialize_str(self, v: &str) -> Result<Value> {
        Ok(Value::Str(v.to_owned()))
    }

    fn serialize_bytes(self, v: &[u8]) -> Result<Value> {
        Ok(Value::Array(
            v.iter().map(|b| Value::UInt(u64::from(*b))).collect(),
        ))
    }

    fn serialize_none(self) -> Result<Value> {
        Ok(Value::Undef)
    }

    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<Value> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<Value> {
        Ok(Value::Undef)
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<Value> {
        Ok(Value::Undef)
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<Value> {
        Ok(Value::Str(variant.to_owned()))
    }

    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Value> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Value> {
        Ok(Value::Hash(vec![(
            variant.to_owned(),
            value.serialize(self)?,
        )]))
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<SeqSerializer> {
        Ok(SeqSerializer {
            items: Vec::with_capacity(len.unwrap_or(0)),
        })
    }

    fn serialize_tuple(self, len: usize) -> Result<SeqSerializer> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_struct(self, _name: &'static str, len: usize) -> Result<SeqSerializer> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<VariantSerializer<SeqSerializer>> {
        Ok(VariantSerializer {
            variant,
            inner: self.serialize_seq(Some(len))?,
        })
    }

    fn serialize_map(self, len: Option<usize>) -> Result<MapSerializer> {
        Ok(MapSerializer {
            entries: Vec::with_capacity(len.unwrap_or(0)),
            key: None,
        })
    }

    fn serialize_struct(self, _name: &'static str, len: usize) -> Result<MapSerializer> {
        self.serialize_map(Some(len))
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<VariantSerializer<MapSerializer>> {
        Ok(VariantSerializer {
            variant,
            inner: self.serialize_map(Some(len))?,
        })
    }

    fn is_human_readable(&self) -> bool {
        true
    }
}

struct SeqSerializer {
    items: Vec<Value>,
}

impl SeqSerializer {
    fn push<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        self.items.push(value.serialize(ValueSerializer)?);
        Ok(())
    }
}

impl ser::SerializeSeq for SeqSerializer {
    type Ok = Value;
    type Error = Error;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        self.push(value)
    }

    fn end(self) -> Result<Value> {
        Ok(Value::Array(self.items))
    }
}

impl ser::SerializeTuple for SeqSerializer {
    type Ok = Value;
    type Error = Error;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        self.push(value)
    }

    fn end(self) -> Result<Value> {
        Ok(Value::Array(self.items))
    }
}

impl ser::SerializeTupleStruct for SeqSerializer {
    type Ok = Value;
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        self.push(value)
    }

    fn end(self) -> Result<Value> {
        Ok(Value::Array(self.items))
    }
}

struct MapSerializer {
    entries: Vec<(String, Value)>,
    key: Option<String>,
}

impl MapSerializer {
    fn field<T: ?Sized + Serialize>(&mut self, key: &str, value: &T) -> Result<()> {
        self.entries
            .push((key.to_owned(), value.serialize(ValueSerializer)?));
        Ok(())
    }
}

impl ser::SerializeMap for MapSerializer {
    type Ok = Value;
    type Error = Error;

    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<()> {
        self.key = Some(key.serialize(KeySerializer)?);
        Ok(())
    }

    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        let key = self
            .key
            .take()
            .ok_or_else(|| ser::Error::custom("value without key"))?;
        self.field(&key, value)
    }

    fn end(self) -> Result<Value> {
        Ok(Value::Hash(self.entries))
    }
}

impl ser::SerializeStruct for MapSerializer {
    type Ok = Value;
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<()> {
        self.field(key, value)
    }

    fn end(self) -> Result<Value> {
        Ok(Value::Hash(self.entries))
    }
}

/// Tuple and struct variants: `{ Variant => [...] }` / `{ Variant => {...} }`.
struct VariantSerializer<S> {
    variant: &'static str,
    inner: S,
}

impl ser::SerializeTupleVariant for VariantSerializer<SeqSerializer> {
    type Ok = Value;
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        self.inner.push(value)
    }

    fn end(self) -> Result<Value> {
        Ok(Value::Hash(vec![(
            self.variant.to_owned(),
            Value::Array(self.inner.items),
        )]))
    }
}

impl ser::SerializeStructVariant for VariantSerializer<MapSerializer> {
    type Ok = Value;
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<()> {
        self.inner.field(key, value)
    }

    fn end(self) -> Result<Value> {
        Ok(Value::Hash(vec![(
            self.variant.to_owned(),
            Value::Hash(self.inner.entries),
        )]))
    }
}

/// Hash keys are always strings in Perl, so keys must serialize as something
/// with a natural string form.
struct KeySerializer;

fn bad_key<T>() -> Result<T> {
    Err(ser::Error::custom(
        "hash keys must be strings, numbers, booleans or characters",
    ))
}

impl ser::Serializer for KeySerializer {
    type Ok = String;
    type Error = Error;

    type SerializeSeq = Impossible<String, Error>;
    type SerializeTuple = Impossible<String, Error>;
    type SerializeTupleStruct = Impossible<String, Error>;
    type SerializeTupleVariant = Impossible<String, Error>;
    type SerializeMap = Impossible<String, Error>;
    type SerializeStruct = Impossible<String, Error>;
    type SerializeStructVariant = Impossible<String, Error>;

    fn serialize_str(self, v: &str) -> Result<String> {
        Ok(v.to_owned())
    }

    fn serialize_char(self, v: char) -> Result<String> {
        Ok(v.to_string())
    }

    fn serialize_bool(self, v: bool) -> Result<String> {
        Ok(if v { "1" } else { "0" }.to_owned())
    }

    fn serialize_i8(self, v: i8) -> Result<String> {
        Ok(v.to_string())
    }

    fn serialize_i16(self, v: i16) -> Result<String> {
        Ok(v.to_string())
    }

    fn serialize_i32(self, v: i32) -> Result<String> {
        Ok(v.to_string())
    }

    fn serialize_i64(self, v: i64) -> Result<String> {
        Ok(v.to_string())
    }

    fn serialize_i128(self, v: i128) -> Result<String> {
        Ok(v.to_string())
    }

    fn serialize_u8(self, v: u8) -> Result<String> {
        Ok(v.to_string())
    }

    fn serialize_u16(self, v: u16) -> Result<String> {
        Ok(v.to_string())
    }

    fn serialize_u32(self, v: u32) -> Result<String> {
        Ok(v.to_string())
    }

    fn serialize_u64(self, v: u64) -> Result<String> {
        Ok(v.to_string())
    }

    fn serialize_u128(self, v: u128) -> Result<String> {
        Ok(v.to_string())
    }

    fn serialize_f32(self, v: f32) -> Result<String> {
        self.serialize_f64(f64::from(v))
    }

    fn serialize_f64(self, v: f64) -> Result<String> {
        Ok(float_text(v, true))
    }

    fn serialize_bytes(self, _v: &[u8]) -> Result<String> {
        bad_key()
    }

    fn serialize_none(self) -> Result<String> {
        bad_key()
    }

    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<String> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<String> {
        bad_key()
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<String> {
        bad_key()
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<String> {
        Ok(variant.to_owned())
    }

    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<String> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<String> {
        bad_key()
    }

    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq> {
        bad_key()
    }

    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple> {
        bad_key()
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct> {
        bad_key()
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant> {
        bad_key()
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap> {
        bad_key()
    }

    fn serialize_struct(self, _name: &'static str, _len: usize) -> Result<Self::SerializeStruct> {
        bad_key()
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant> {
        bad_key()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[test]
    fn test_struct() {
        #[derive(Serialize)]
        struct Test {
            int: u32,
            seq: Vec<&'static str>,
        }
        let test = Test {
            int: 1,
            seq: vec!["a", "b"],
        };
        assert_eq!(to_string(&test).unwrap(), r#"{int=>1,seq=>['a','b']}"#);
    }

    #[test]
    fn test_enum() {
        #[derive(Serialize)]
        enum E {
            Unit,
            Newtype(u32),
            Tuple(u32, u32),
            Struct { a: u32 },
            Undefined { val: Option<u32> },
        }
        let s = |e: E| to_string(&e).unwrap();
        assert_eq!(
            s(E::Undefined { val: None }),
            r#"{Undefined=>{val=>undef}}"#
        );
        assert_eq!(s(E::Unit), r#"'Unit'"#);
        assert_eq!(s(E::Newtype(1)), r#"{Newtype=>1}"#);
        assert_eq!(s(E::Tuple(1, 2)), r#"{Tuple=>[1,2]}"#);
        assert_eq!(s(E::Struct { a: 1 }), r#"{Struct=>{a=>1}}"#);
    }

    #[test]
    fn test_keys_and_specials() {
        let m: std::collections::BTreeMap<i32, &str> = [(1, "a"), (2, "b")].into();
        assert_eq!(to_string(&m).unwrap(), "{1=>'a',2=>'b'}");
        let m: std::collections::BTreeMap<&str, i32> = [("007", 1), ("a b", 2), ("ok", 3)].into();
        assert_eq!(to_string(&m).unwrap(), "{'007'=>1,'a b'=>2,ok=>3}");
        assert_eq!(to_string(&f64::NAN).unwrap(), "'NaN'");
        assert_eq!(to_string(&f64::INFINITY).unwrap(), "'Inf'");
        assert_eq!(to_string(&f64::NEG_INFINITY).unwrap(), "'-Inf'");
        assert_eq!(to_string(&0.1f32).unwrap(), "'0.1'");
        assert_eq!(to_string(&u64::MAX).unwrap(), "'18446744073709551615'");
        assert_eq!(to_string(&(1u8, 'x', true, ())).unwrap(), "[1,'x',1,undef]");
        assert!(to_string(&std::collections::BTreeMap::from([(vec![1], 1)])).is_err());
    }
}
