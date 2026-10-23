//! Perl config files are often nothing but `$name = value;` statements.

use serde::Deserialize;
use std::collections::BTreeMap;

const CONFIG: &str = r#"# Settings
#
# Anything after a hash sign is a comment.

# Is this thing on? Use 1 or 0.
$enabled = 1;

# The group to run as.
$group = 'www-data';

$driver = 'mysql';
$host = 'db.example.com';   # trailing comment
$port = 3306;
$socket = '';
$password = 'it\'s a \\ secret';
$ratio = '0.75';
$paths = ['/usr/bin', '/usr/local/bin'];
$limits = { cpu => 2, 'mem mb' => 512 };
"#;

#[derive(Debug, Deserialize, PartialEq)]
struct Config {
    enabled: bool,
    group: String,
    port: u16,
    socket: String,
    password: String,
    ratio: f64,
    paths: Vec<String>,
    limits: BTreeMap<String, u32>,
    // `driver` and `host` are in the file but not listed here
}

#[test]
fn deserializes_into_a_struct() {
    let c: Config = serde_perl_dumper::from_assignments(CONFIG).unwrap();
    assert_eq!(
        c,
        Config {
            enabled: true,
            group: "www-data".into(),
            port: 3306,
            socket: "".into(),
            password: "it's a \\ secret".into(),
            ratio: 0.75,
            paths: vec!["/usr/bin".into(), "/usr/local/bin".into()],
            limits: [("cpu".to_string(), 2), ("mem mb".to_string(), 512)].into(),
        }
    );
}

#[test]
fn lists_every_assignment_in_order() {
    let vars = serde_perl_dumper::parse_assignments(CONFIG).unwrap();
    let names: Vec<&str> = vars.iter().map(|(n, _)| &**n).collect();
    assert_eq!(
        names,
        [
            "enabled", "group", "driver", "host", "port", "socket", "password", "ratio", "paths",
            "limits"
        ]
    );
}

#[test]
fn into_a_map_and_last_assignment_wins() {
    let m: BTreeMap<String, String> =
        serde_perl_dumper::from_assignments("$a = 'x'; $a = 'y'; $b = 2;").unwrap();
    assert_eq!(m["a"], "y");
    assert_eq!(m["b"], "2");
}

#[test]
fn declarations_and_empty_files() {
    let m: BTreeMap<String, i32> =
        serde_perl_dumper::from_assignments("my $a = 1;\nour $b = 2;\n").unwrap();
    assert_eq!((m["a"], m["b"]), (1, 2));
    let empty: BTreeMap<String, i32> =
        serde_perl_dumper::from_assignments("# nothing here\n").unwrap();
    assert!(empty.is_empty());
}

#[test]
fn errors_point_at_the_problem() {
    let err =
        serde_perl_dumper::from_assignments::<BTreeMap<String, String>>("$a = 'x';\n$b = ;\n")
            .unwrap_err();
    assert!(err.to_string().contains("line 2"), "{err}");
    assert!(serde_perl_dumper::parse_assignments("$a = 1 $b = 2;").is_err());
    assert!(serde_perl_dumper::parse_assignments("1;").is_err());
}

/// The config file evaluates to the same values in Perl itself.
#[test]
fn agrees_with_perl() {
    let code = format!(
        "{CONFIG} print join('|', $enabled, $group, $port, $password, $ratio, scalar(@$paths), $limits->{{'mem mb'}});"
    );
    let Ok(out) = std::process::Command::new("perl")
        .args(["-e", &code])
        .output()
    else {
        return;
    };
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "1|www-data|3306|it's a \\ secret|0.75|2|512"
    );
}
