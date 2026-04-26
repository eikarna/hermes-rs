use criterion::{criterion_group, criterion_main, Criterion};
use hermes_core::parser::ToolCallStreamParser;

fn benchmark_parser(c: &mut Criterion) {
    let mut group = c.benchmark_group("parser");

    let text = "This is a large stream of text that is being parsed incrementally by the parser. "
        .repeat(100);

    group.bench_function("parse_large_text", |b| {
        b.iter(|| {
            let mut parser = ToolCallStreamParser::new();
            for chunk in text.split_whitespace() {
                parser.process_chunk(chunk);
                parser.process_chunk(" ");
            }
            // black_box(parser.take_text());
        });
    });

    group.finish();
}

criterion_group!(benches, benchmark_parser);
criterion_main!(benches);
