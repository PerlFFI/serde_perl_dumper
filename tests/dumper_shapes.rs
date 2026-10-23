//! Shapes that show up in real `Data::Dumper` output and need a little help
//! from the target types: Perl's "ordered hash" lists and values that are
//! sometimes numbers and sometimes strings.

use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt;

const DUMP: &str = r#"$VAR1 = {
          'users' => {
                       'FIELDS' => [
                                     'id',
                                     {
                                       'DEFAULT' => 0,
                                       'NOTNULL' => 1,
                                       'PRIMARYKEY' => 1,
                                       'TYPE' => 'INT3'
                                     },
                                     'name',
                                     {
                                       'DEFAULT' => '\'\'',
                                       'TYPE' => 'varchar(64)'
                                     },
                                     'active',
                                     {
                                       'DEFAULT' => 'TRUE',
                                       'NOTNULL' => 1,
                                       'TYPE' => 'BOOLEAN'
                                     }
                                   ],
                       'INDEXES' => [
                                      'users_name_idx',
                                      [
                                        'name'
                                      ],
                                      'users_unique_idx',
                                      {
                                        'FIELDS' => [
                                                      'id',
                                                      'name'
                                                    ],
                                        'TYPE' => 'UNIQUE'
                                      }
                                    ]
                     },
          'empty' => {
                       'FIELDS' => []
                     }
        };
"#;

#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
struct Table {
    /// Perl "ordered hash": `[ name, def, name, def, ... ]`.
    #[serde(deserialize_with = "pairs")]
    fields: Vec<(String, Column)>,
    #[serde(default, deserialize_with = "pairs")]
    indexes: Vec<(String, Index)>,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
struct Column {
    #[serde(rename = "TYPE")]
    ty: String,
    #[serde(default)]
    notnull: bool,
    #[serde(default)]
    primarykey: bool,
    default: Option<Default>,
}

/// The same column default shows up as `0`, `'0'` and `'TRUE'`.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(untagged)]
enum Default {
    Int(i64),
    Text(String),
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(untagged)]
enum Index {
    Columns(Vec<String>),
    Detailed {
        #[serde(rename = "FIELDS")]
        fields: Vec<String>,
        #[serde(rename = "TYPE")]
        ty: IndexType,
    },
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
enum IndexType {
    Unique,
    Fulltext,
}

/// Deserialize a flat `[ k, v, k, v, ... ]` list into pairs, keeping order.
fn pairs<'de, D, V>(d: D) -> Result<Vec<(String, V)>, D::Error>
where
    D: Deserializer<'de>,
    V: Deserialize<'de>,
{
    struct Pairs<V>(std::marker::PhantomData<V>);

    impl<'de, V: Deserialize<'de>> Visitor<'de> for Pairs<V> {
        type Value = Vec<(String, V)>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a list of alternating names and definitions")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(name) = seq.next_element::<String>()? {
                let def = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::custom(format!("{name:?} has no definition")))?;
                out.push((name, def));
            }
            Ok(out)
        }
    }

    d.deserialize_seq(Pairs(std::marker::PhantomData))
}

#[test]
fn ordered_lists_and_untagged_enums() {
    let tables: BTreeMap<String, Table> = serde_perl_dumper::from_str(DUMP).unwrap();
    let users = &tables["users"];

    let names: Vec<&str> = users.fields.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["id", "name", "active"]);

    let (_, id) = &users.fields[0];
    assert_eq!(id.ty, "INT3");
    assert!(id.notnull && id.primarykey);
    assert_eq!(id.default, Some(Default::Int(0)));
    assert_eq!(users.fields[1].1.default, Some(Default::Text("''".into())));
    assert_eq!(
        users.fields[2].1.default,
        Some(Default::Text("TRUE".into()))
    );

    assert_eq!(
        users.indexes[0],
        ("users_name_idx".into(), Index::Columns(vec!["name".into()]))
    );
    assert_eq!(
        users.indexes[1],
        (
            "users_unique_idx".into(),
            Index::Detailed {
                fields: vec!["id".into(), "name".into()],
                ty: IndexType::Unique
            }
        )
    );

    // a table without INDEXES, and with no columns
    assert_eq!(
        tables["empty"],
        Table {
            fields: vec![],
            indexes: vec![]
        }
    );
}

#[test]
fn tree_and_streaming_agree() {
    let streamed: BTreeMap<String, Table> = serde_perl_dumper::from_str(DUMP).unwrap();
    let tree = serde_perl_dumper::parse(DUMP).unwrap();
    let via_tree: BTreeMap<String, Table> = serde_perl_dumper::from_perl(&tree).unwrap();
    assert_eq!(streamed, via_tree);
}

#[test]
fn odd_length_lists_are_an_error() {
    let err = serde_perl_dumper::from_str::<Table>("{ FIELDS => ['id'] }").unwrap_err();
    assert!(err.to_string().contains("has no definition"), "{err}");
}
