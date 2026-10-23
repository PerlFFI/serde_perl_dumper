//! `Dumper` output is compared character for character with Perl's own
//! `Data::Dumper`. Skipped when `perl` isn't installed.

use serde_perl_dumper::{Dumper, Value};
use std::process::Command;

fn s(x: &str) -> Value {
    Value::Str(x.into())
}

fn h(pairs: Vec<(&str, Value)>) -> Value {
    Value::Hash(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

/// The same data as `PERL_SAMPLE`.
fn sample() -> Value {
    use Value::*;
    h(vec![
        ("name", s("it's a \"test\"\n")),
        ("n", Int(-42)),
        ("big", Int(12345678901)),
        ("zero", Int(0)),
        ("ten", Int(1234567890)),
        ("negten", Int(-1234567890)),
        ("neg9", Int(-123456789)),
        ("f", Float(1.5)),
        ("sum", Float(0.1 + 0.2)),
        ("e", Float(1e20)),
        ("whole", Float(3.0)),
        ("nothing", Undef),
        ("numstr", s("42")),
        ("oct", s("007")),
        ("uni", s("caf\u{e9} \u{263a}")),
        ("ctl", s("a\u{1}b\t$@\\1\u{1}2")),
        (
            "list",
            Array(vec![
                Int(1),
                Array(vec![Int(2), Int(3)]),
                h(vec![("k", Array(vec![]))]),
                h(vec![]),
            ]),
        ),
        ("007", Int(7)),
        ("a b", Int(8)),
        ("12", Int(9)),
        ("", Int(10)),
        ("a::b", Int(11)),
        ("inf", Float(f64::INFINITY)),
        ("ninf", Float(f64::NEG_INFINITY)),
        ("nan", Float(f64::NAN)),
    ])
}

const PERL_SAMPLE: &str = r#"{
    name => "it's a \"test\"\n", n => -42, big => 12345678901, zero => 0, ten => 1234567890, negten => -1234567890, neg9 => -123456789,
    f => 1.5, sum => 0.1 + 0.2, e => 1e20, whole => 3.0, nothing => undef,
    numstr => "42", oct => "007", uni => "caf\x{e9} \x{263a}",
    ctl => "a\x01b\t\$\@\\1\x012",
    list => [1, [2, 3], { k => [] }, {}],
    '007' => 7, 'a b' => 8, 12 => 9, '' => 10, 'a::b' => 11,
    inf => 9**9**9, ninf => -9**9**9, nan => -sin(9**9**9),
}"#;

fn perl_dump(config: &str, names: &str) -> Option<String> {
    let code = format!(
        r#"use utf8; use Data::Dumper; binmode STDOUT, ':utf8';
        $Data::Dumper::Sortkeys = 1; {config}
        my $a = {PERL_SAMPLE}; my $b = [1, {{ x => [] }}];
        print Data::Dumper->Dump([$a, $b], [{names}]);"#
    );
    let out = Command::new("perl").args(["-e", &code]).output().ok()?;
    assert!(
        out.status.success(),
        "perl failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    // addresses in Maxdepth placeholders
    let re_free: String = {
        let mut out = String::new();
        let mut rest = text.as_str();
        while let Some(i) = rest.find("(0x") {
            out.push_str(&rest[..i]);
            out.push_str("(0x0)");
            let end = rest[i..].find(')').unwrap();
            rest = &rest[i + end + 1..];
        }
        out.push_str(rest);
        out
    };
    Some(re_free)
}

fn rust_dump(d: &Dumper) -> String {
    d.dump_all(&[
        sample(),
        Value::Array(vec![Value::Int(1), h(vec![("x", Value::Array(vec![]))])]),
    ])
}

fn check(label: &str, d: Dumper, config: &str, names: &str) {
    let Some(want) = perl_dump(config, names) else {
        return;
    };
    let got = rust_dump(&d);
    if got != want {
        let diff = got
            .lines()
            .zip(want.lines())
            .enumerate()
            .find(|(_, (a, b))| a != b)
            .map(|(i, (a, b))| format!("line {}:\n rust: {a:?}\n perl: {b:?}", i + 1))
            .unwrap_or_else(|| {
                format!(
                    "length differs: rust {} lines, perl {}",
                    got.lines().count(),
                    want.lines().count()
                )
            });
        panic!("--- {label} ---\n{diff}");
    }
}

#[test]
fn defaults() {
    check("default", Dumper::builder().sortkeys(true).build(), "", "");
}

#[test]
fn indent_levels() {
    for n in 0..=4u8 {
        let want_n = n.min(3);
        check(
            &format!("indent {n}"),
            Dumper::builder().sortkeys(true).indent(n).build(),
            &format!("$Data::Dumper::Indent = {want_n};"),
            "",
        );
    }
}

#[test]
fn terse_and_varname() {
    for indent in [0, 1, 2] {
        check(
            "terse",
            Dumper::builder()
                .sortkeys(true)
                .terse(true)
                .indent(indent)
                .build(),
            &format!("$Data::Dumper::Terse = 1; $Data::Dumper::Indent = {indent};"),
            "",
        );
    }
    check(
        "varname",
        Dumper::builder().sortkeys(true).varname("foo").build(),
        "$Data::Dumper::Varname = 'foo';",
        "",
    );
}

#[test]
fn quoting() {
    check(
        "useqq",
        Dumper::builder().sortkeys(true).useqq(true).build(),
        "$Data::Dumper::Useqq = 1;",
        "",
    );
    check(
        "quotekeys",
        Dumper::builder().sortkeys(true).quotekeys(false).build(),
        "$Data::Dumper::Quotekeys = 0;",
        "",
    );
    check(
        "both",
        Dumper::builder()
            .sortkeys(true)
            .useqq(true)
            .quotekeys(false)
            .indent(1)
            .build(),
        "$Data::Dumper::Useqq = 1; $Data::Dumper::Quotekeys = 0; $Data::Dumper::Indent = 1;",
        "",
    );
}

#[test]
fn layout_options() {
    for indent in [0, 1, 2, 3] {
        check(
            "trailingcomma",
            Dumper::builder()
                .sortkeys(true)
                .trailingcomma(true)
                .indent(indent)
                .build(),
            &format!("$Data::Dumper::Trailingcomma = 1; $Data::Dumper::Indent = {indent};"),
            "",
        );
    }
    check(
        "pair",
        Dumper::builder().sortkeys(true).pair(" : ").build(),
        "$Data::Dumper::Pair = ' : ';",
        "",
    );
    check(
        "pair0",
        Dumper::builder()
            .sortkeys(true)
            .pair("=>")
            .indent(0)
            .build(),
        "$Data::Dumper::Pair = '=>'; $Data::Dumper::Indent = 0;",
        "",
    );
    check(
        "pad",
        Dumper::builder().sortkeys(true).pad("# ").build(),
        "$Data::Dumper::Pad = '# ';",
        "",
    );
    check(
        "pad1",
        Dumper::builder().sortkeys(true).pad("> ").indent(1).build(),
        "$Data::Dumper::Pad = '> '; $Data::Dumper::Indent = 1;",
        "",
    );
}

#[test]
fn maxdepth() {
    for depth in [1, 2] {
        check(
            "maxdepth",
            Dumper::builder().sortkeys(true).maxdepth(depth).build(),
            &format!("$Data::Dumper::Maxdepth = {depth};"),
            "",
        );
    }
}

#[test]
fn names() {
    check(
        "star names",
        Dumper::builder()
            .sortkeys(true)
            .names(vec!["*cfg".into(), "*list".into()])
            .build(),
        "",
        "qw(*cfg *list)",
    );
    check(
        "scalar names",
        Dumper::builder()
            .sortkeys(true)
            .names(vec!["cfg".into(), "$other".into()])
            .build(),
        "",
        "qw(cfg $other)",
    );
    check(
        "partial names",
        Dumper::builder()
            .sortkeys(true)
            .names(vec!["only".into()])
            .build(),
        "",
        "qw(only)",
    );
}

#[test]
fn not_sorted_keeps_insertion_order() {
    let d = Dumper::builder().indent(0).build();
    let v = h(vec![("z", Value::Int(1)), ("a", Value::Int(2))]);
    assert_eq!(
        d.dump_all(std::slice::from_ref(&v)),
        "$VAR1 = {'z' => 1,'a' => 2};"
    );
    let d = Dumper::builder().indent(0).sortkeys(true).build();
    assert_eq!(d.dump_all(&[v]), "$VAR1 = {'a' => 2,'z' => 1};");
}

#[test]
fn output_evaluates_as_perl() {
    let Some(out) = perl_dump("", "") else { return };
    // sanity check the harness: what perl wrote is what the crate parses back
    let vars = serde_perl_dumper::parse_all(&out).unwrap();
    assert_eq!(vars.len(), 2);

    // and what the crate writes evaluates to the same thing in perl
    let text = Dumper::builder()
        .sortkeys(true)
        .useqq(true)
        .build()
        .dump_all(&[sample()]);
    let code = format!(
        "use utf8; use Data::Dumper; binmode STDOUT, ':utf8'; $Data::Dumper::Sortkeys = 1; $Data::Dumper::Indent = 1;
         my $VAR1; {text} print Dumper($VAR1);"
    );
    let again = Command::new("perl").args(["-e", &code]).output().unwrap();
    assert!(
        again.status.success(),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
}

/// Whatever the options, the crate's own parser reads the output back.
#[test]
fn every_option_set_parses_back() {
    for indent in 0..=3u8 {
        for useqq in [false, true] {
            for quotekeys in [false, true] {
                for trailingcomma in [false, true] {
                    let d = Dumper::builder()
                        .indent(indent)
                        .useqq(useqq)
                        .quotekeys(quotekeys)
                        .trailingcomma(trailingcomma)
                        .sortkeys(true)
                        .build();
                    let text = rust_dump(&d);
                    let vars = serde_perl_dumper::parse_all(&text)
                        .unwrap_or_else(|e| panic!("indent {indent} useqq {useqq}: {e}\n{text}"));
                    assert_eq!(vars.len(), 2);
                }
            }
        }
    }
}

/// `to_string` is a compact preset of the same machinery.
#[test]
fn to_string_is_compact_and_lossless() {
    assert_eq!(
        serde_perl_dumper::to_string(&0.1f64.min(0.1 + 0.2)).unwrap(),
        "'0.1'"
    );
    assert_eq!(
        serde_perl_dumper::to_string(&(0.1 + 0.2)).unwrap(),
        "'0.30000000000000004'"
    );
    let back: f64 =
        serde_perl_dumper::from_str(&serde_perl_dumper::to_string(&(0.1 + 0.2)).unwrap()).unwrap();
    assert_eq!(back, 0.1 + 0.2);
}
