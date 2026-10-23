use serde::Serialize;

use crate::error::Result;
use crate::quote::{
    double_quote, float_text, is_bare_key, is_safe_int_str, is_short_int, single_quote, wide_quote,
};
use crate::ser::{to_value, Value};

/// Writes Perl source the way `Data::Dumper` does, with most of its
/// configuration variables as builder options.
///
/// ```
/// use serde_perl_dumper::Dumper;
///
/// let dumper = Dumper::builder().indent(1).sortkeys(true).build();
/// let text = dumper.dump(&vec![("b", 2), ("a", 1)]).unwrap();
/// assert_eq!(text, "$VAR1 = [\n  [\n    'b',\n    2\n  ],\n  [\n    'a',\n    1\n  ]\n];\n");
/// ```
///
/// Each option is named after, and defaults to, the matching
/// `$Data::Dumper::...` variable. Not provided, because they have no meaning
/// without Perl references and objects: `Purity`, `Deepcopy`, `Freezer`,
/// `Toaster`, `Bless`, `Deparse`, `Sparseseen`, `Useperl`, `Maxrecurse`.
#[derive(Debug, Clone, bon::Builder)]
pub struct Dumper {
    /// `Indent`: 0 is one line; 1 indents by two spaces per level; 2 lines
    /// children up under their opening bracket; 3 is like 2 plus `#index`
    /// comments before array elements. Larger values act as 3.
    #[builder(default = 2)]
    indent: u8,
    /// `Terse`: leave out the `$VAR1 = ` and the `;`.
    #[builder(default)]
    terse: bool,
    /// `Sortkeys`: write hash keys in sorted order instead of the order they
    /// were serialized in.
    #[builder(default)]
    sortkeys: bool,
    /// `Useqq`: double-quoted strings with escapes, instead of single quotes.
    #[builder(default)]
    useqq: bool,
    /// `Quotekeys`: quote every hash key. When false, identifiers and plain
    /// integers are written bare.
    #[builder(default = true)]
    quotekeys: bool,
    /// `Trailingcomma`: put a comma after the last element when the closing
    /// bracket is on its own line.
    #[builder(default)]
    trailingcomma: bool,
    /// `Maxdepth`: containers nested deeper than this are replaced by a
    /// `'HASH(0x0)'` / `'ARRAY(0x0)'` placeholder string. 0 means no limit.
    #[builder(default)]
    maxdepth: usize,
    /// `Pair`: what goes between a key and its value.
    #[builder(into, default = " => ")]
    pair: String,
    /// `Varname`: prefix for the generated names (`$VAR1`, `$VAR2`, ...).
    #[builder(into, default = "VAR")]
    varname: String,
    /// `Pad`: put this at the start of every line.
    #[builder(into, default)]
    pad: String,
    /// The `names` argument of `Data::Dumper->Dump`. Entry *i* names the
    /// *i*th value: `foo` or `$foo` gives `$foo = ...;`, and `*foo` writes a
    /// hash as `%foo = (...);` and an array as `@foo = (...);`. Empty entries
    /// and missing ones fall back to `Varname`.
    #[builder(default)]
    names: Vec<String>,
    /// Not a `Data::Dumper` option. Perl writes floats with 15 significant
    /// digits, so `0.1 + 0.2` becomes `'0.3'`. Set this to write the shortest
    /// text that reads back as the same `f64`.
    #[builder(default)]
    lossless_floats: bool,
}

impl Default for Dumper {
    fn default() -> Self {
        Dumper::builder().build()
    }
}

impl Dumper {
    /// Dump one value as `$VAR1 = ...;` (or a bare expression if `terse`).
    pub fn dump<T>(&self, value: &T) -> Result<String>
    where
        T: Serialize + ?Sized,
    {
        Ok(self.dump_all(&[to_value(value)?]))
    }

    /// Dump several values, numbering them `$VAR1`, `$VAR2`, ... like
    /// `Dumper($a, $b)`.
    pub fn dump_all(&self, values: &[Value]) -> String {
        let mut f = Formatter {
            d: self,
            out: String::new(),
            content_start: 0,
        };
        for (i, value) in values.iter().enumerate() {
            f.top(i, value);
        }
        f.out
    }
}

struct Formatter<'a> {
    d: &'a Dumper,
    out: String,
    /// Where the current line's text starts, after the pad.
    content_start: usize,
}

impl Formatter<'_> {
    fn level(&self) -> u8 {
        self.d.indent.min(3)
    }

    fn start_line(&mut self) {
        self.out.push_str(&self.d.pad);
        self.content_start = self.out.len();
    }

    fn newline(&mut self, indent: usize) {
        self.out.push('\n');
        self.start_line();
        self.out.extend(std::iter::repeat_n(' ', indent));
    }

    fn top(&mut self, index: usize, value: &Value) {
        self.start_line();
        let mut paren = false;
        let mut close_tail = ";";
        if !self.d.terse {
            let name = self.d.names.get(index).map(String::as_str).unwrap_or("");
            let (sigil, name) = match name.strip_prefix('*') {
                Some(n) if !n.is_empty() => match value {
                    Value::Hash(_) => {
                        paren = true;
                        ('%', n.to_owned())
                    }
                    Value::Array(_) => {
                        paren = true;
                        ('@', n.to_owned())
                    }
                    _ => ('$', n.to_owned()),
                },
                _ if !name.is_empty() => ('$', name.trim_start_matches(['$', '*']).to_owned()),
                _ => ('$', format!("{}{}", self.d.varname, index + 1)),
            };
            self.out.push(sigil);
            self.out.push_str(&name);
            self.out.push_str(" = ");
        } else {
            close_tail = "";
        }
        let apad = self.out[self.content_start..].chars().count();
        self.value(value, 0, apad, paren);
        self.out.push_str(close_tail);
        if self.level() > 0 {
            self.out.push('\n');
        }
    }

    fn value(&mut self, value: &Value, depth: usize, apad: usize, paren: bool) {
        match value {
            Value::Undef => self.out.push_str("undef"),
            Value::Int(n) => self.int(i128::from(*n)),
            Value::UInt(n) => self.int(i128::from(*n)),
            Value::Float(f) => {
                let text = float_text(*f, self.d.lossless_floats);
                self.text_scalar(&text);
            }
            Value::Str(s) => self.text_scalar(s),
            Value::Array(items) => self.array(items, depth, apad, paren),
            Value::Hash(entries) => self.hash(entries, depth, apad, paren),
        }
    }

    fn int(&mut self, n: i128) {
        if is_short_int(n) {
            self.out.push_str(&n.to_string());
        } else {
            single_quote(&mut self.out, &n.to_string());
        }
    }

    /// A string or float. With `Useqq`, ones that look like plain integers are
    /// written bare, as `Data::Dumper` does.
    fn text_scalar(&mut self, text: &str) {
        if self.d.useqq && is_safe_int_str(text) {
            self.out.push_str(text);
        } else {
            self.string(text);
        }
    }

    fn string(&mut self, s: &str) {
        if self.d.useqq {
            double_quote(&mut self.out, s);
        } else if s.is_ascii() {
            single_quote(&mut self.out, s);
        } else {
            wide_quote(&mut self.out, s);
        }
    }

    fn key(&mut self, k: &str) {
        if !self.d.quotekeys && is_bare_key(k) {
            self.out.push_str(k);
        } else {
            self.string(k);
        }
    }

    /// Where children and the closing bracket go. `Data::Dumper` doesn't
    /// measure the real column: it tracks `apad`, which grows by the key's
    /// width plus a fixed 4 for each hash level, whatever `Pair` is.
    fn indents(&self, depth: usize, apad: usize) -> (usize, usize) {
        match self.level() {
            0 => (0, 0),
            1 => ((depth + 1) * 2, depth * 2),
            _ => (apad + 2, apad),
        }
    }

    fn too_deep(&mut self, depth: usize, what: &str) -> bool {
        if self.d.maxdepth > 0 && depth >= self.d.maxdepth {
            single_quote(&mut self.out, &format!("{what}(0x0)"));
            true
        } else {
            false
        }
    }

    fn array(&mut self, items: &[Value], depth: usize, apad: usize, paren: bool) {
        if self.too_deep(depth, "ARRAY") {
            return;
        }
        let (open, close_bracket) = if paren { ('(', ')') } else { ('[', ']') };
        let (child, close) = self.indents(depth, apad);
        self.out.push(open);
        if items.is_empty() {
            self.out.push(close_bracket);
            return;
        }
        let last = items.len() - 1;
        for (i, item) in items.iter().enumerate() {
            if self.level() > 0 {
                self.newline(child);
                if self.level() >= 3 {
                    self.out.push_str(&format!("#{i}"));
                    self.newline(child);
                }
            }
            self.value(item, depth + 1, child, false);
            self.separator(i == last);
        }
        self.close(close, close_bracket);
    }

    fn hash(&mut self, entries: &[(String, Value)], depth: usize, apad: usize, paren: bool) {
        if self.too_deep(depth, "HASH") {
            return;
        }
        let (open, close_bracket) = if paren { ('(', ')') } else { ('{', '}') };
        let (child, close) = self.indents(depth, apad);
        self.out.push(open);
        if entries.is_empty() {
            self.out.push(close_bracket);
            return;
        }
        let mut order: Vec<&(String, Value)> = entries.iter().collect();
        if self.d.sortkeys {
            order.sort_by(|a, b| a.0.cmp(&b.0));
        }
        let last = order.len() - 1;
        for (i, (k, v)) in order.into_iter().enumerate() {
            if self.level() > 0 {
                self.newline(child);
            }
            let key_start = self.out.len();
            self.key(k);
            let key_width = self.out[key_start..].chars().count();
            self.out.push_str(&self.d.pair);
            self.value(v, depth + 1, child + key_width + 4, false);
            self.separator(i == last);
        }
        self.close(close, close_bracket);
    }

    fn separator(&mut self, last: bool) {
        if !last || (self.d.trailingcomma && self.level() > 0) {
            self.out.push(',');
        }
    }

    fn close(&mut self, indent: usize, bracket: char) {
        if self.level() > 0 {
            self.newline(indent);
        }
        self.out.push(bracket);
    }
}
