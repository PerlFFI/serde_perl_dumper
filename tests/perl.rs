//! Cross-checks against a real `perl`; tests are skipped when it is not installed.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::process::Command;

fn perl(code: &str) -> Option<String> {
    let out = Command::new("perl")
        .args(["-MData::Dumper", "-e", code])
        .output()
        .ok()?;
    assert!(
        out.status.success(),
        "perl failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8(out.stdout).unwrap())
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
struct Rec {
    name: String,
    n: i64,
    f: f64,
    tags: Vec<String>,
    opt: Option<String>,
    map: BTreeMap<String, i32>,
}

const SETUP: &str = r#"
    $Data::Dumper::Sortkeys = 1;
    my $r = { name => "it's a \\ \n \x{263a} caf\x{e9}", n => -42, f => 1.5, tags => ["a", "", "007"],
              opt => undef, map => { 1 => 2, x => 3 } };
"#;

#[test]
fn dumper_default_indent() {
    let Some(text) = perl(&format!("{SETUP} print Dumper(bless $r, 'Some::Class')")) else {
        return;
    };
    let rec: Rec = serde_perl_dumper::from_str(&text).unwrap();
    assert_eq!(rec.name, "it's a \\ \n \u{263a} caf\u{e9}");
    assert_eq!((rec.n, rec.f, rec.opt.as_deref()), (-42, 1.5, None));
    assert_eq!(rec.tags, ["a", "", "007"]);
    assert_eq!(rec.map["1"], 2);
}

#[test]
fn dumper_variants() {
    for cfg in [
        "$Data::Dumper::Indent=0",
        "$Data::Dumper::Indent=1",
        "$Data::Dumper::Terse=1",
        "$Data::Dumper::Useqq=1",
        "$Data::Dumper::Useqq=1; $Data::Dumper::Indent=0",
        "$Data::Dumper::Deparse=0; $Data::Dumper::Quotekeys=0",
    ] {
        let Some(text) = perl(&format!("{SETUP} {cfg}; print Dumper($r)")) else {
            return;
        };
        let rec: Rec = match serde_perl_dumper::from_str(&text) {
            Ok(r) => r,
            Err(e) => panic!("{cfg}: {e}\n{text}"),
        };
        assert_eq!(rec.name, "it's a \\ \n \u{263a} caf\u{e9}", "{cfg}");
    }
}

#[test]
fn dumper_backrefs_and_specials() {
    // code refs are not supported
    if let Some(text) = perl("print Dumper({ c => sub { 1 } })") {
        assert!(serde_perl_dumper::parse(&text).is_err());
    }

    #[derive(Deserialize)]
    struct T {
        a: Vec<i32>,
        b: Vec<i32>,
        r: String,
        s: String,
    }
    let Some(text) = perl(
        "$Data::Dumper::Sortkeys=1; my $a=[1,2]; print Dumper({ a=>$a, b=>$a, r=>qr/x+/i, s=>\\'q' })",
    ) else { return };
    // the streaming path refuses back-references...
    assert!(serde_perl_dumper::from_str::<T>(&text).is_err());
    // ...which the caller resolves explicitly.
    let vars =
        serde_perl_dumper::resolve_backrefs(serde_perl_dumper::parse_all(&text).unwrap()).unwrap();
    let t: T = serde_perl_dumper::from_perl(&vars[0]).unwrap();
    assert_eq!(
        (t.a, t.b, t.r, t.s),
        (vec![1, 2], vec![1, 2], "x+".into(), "q".into())
    );
}

#[test]
fn rust_output_is_valid_perl() {
    let rec = Rec {
        name: "it's \\ \n \u{263a} \\\\ end\\".into(),
        n: -7,
        f: 2.5,
        tags: vec!["x".into(), "".into()],
        opt: None,
        map: [("a b".to_string(), 1)].into(),
    };
    let text = serde_perl_dumper::to_string(&rec).unwrap();
    let Some(out) = perl(&format!(
        "use utf8; binmode STDOUT, ':utf8'; my $v = {text}; print join('|', $v->{{name}}, $v->{{n}}, $v->{{f}}, scalar(@{{$v->{{tags}}}}), defined($v->{{opt}}) ? 'def' : 'undef', $v->{{map}}{{'a b'}})"
    )) else { return };
    assert_eq!(out, "it's \\ \n \u{263a} \\\\ end\\|-7|2.5|2|undef|1");
    let back: Rec = serde_perl_dumper::from_str(&text).unwrap();
    assert_eq!(back, rec);
}
