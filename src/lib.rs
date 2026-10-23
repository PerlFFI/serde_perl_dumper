mod de;
mod dumper;
mod error;
mod parser;
mod quote;
mod ser;

pub use de::{from_assignments, from_perl, from_str, Deserializer};
pub use dumper::Dumper;
pub use error::{Error, Result};
pub use parser::{
    parse, parse_all, parse_assignments, resolve_backrefs, Array, BackRef, Hash, Reference, Scalar,
    Step,
};
pub use ser::{to_string, to_value, Value};
