use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};

/// Build text shaped like `Data::Dumper` output (Indent=2) with a mix of
/// nested hashes/arrays, ints, quoted floats, escaped strings and undef.
fn sample(rows: usize) -> String {
    let mut s = String::from("$VAR1 = [\n");
    for i in 0..rows {
        s.push_str(&format!(
            "          {{\n            'id' => {i},\n            'name' => 'it\\'s row {i} \\\\ ok',\n            'score' => '{}.5',\n            'note' => undef,\n            'tags' => [\n                        'alpha',\n                        'beta',\n                        -{i}\n                      ],\n            'meta' => bless( {{\n                               'k' => 'v',\n                               'empty' => ''\n                             }}, 'Foo::Bar' )\n          }},\n",
            i % 100
        ));
    }
    s.push_str("        ];\n");
    s
}

fn bench(c: &mut Criterion) {
    let text = sample(20_000);
    let mut g = c.benchmark_group("parse");
    g.throughput(Throughput::Bytes(text.len() as u64));
    g.bench_function("parse_all", |b| {
        b.iter(|| serde_perl_dumper::parse_all(black_box(&text)).unwrap())
    });
    g.finish();

    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct Row {
        id: i64,
        name: String,
        score: f64,
        note: Option<String>,
        tags: Vec<String>,
    }
    let mut g = c.benchmark_group("deserialize");
    g.throughput(Throughput::Bytes(text.len() as u64));
    g.bench_function("from_str (streaming)", |b| {
        b.iter(|| serde_perl_dumper::from_str::<Vec<Row>>(black_box(&text)).unwrap())
    });
    g.bench_function("parse + from_perl (tree)", |b| {
        b.iter(|| {
            let scalar = serde_perl_dumper::parse(black_box(&text)).unwrap();
            serde_perl_dumper::from_perl::<Vec<Row>>(&scalar).unwrap()
        })
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
