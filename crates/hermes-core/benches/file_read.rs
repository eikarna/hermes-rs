use criterion::{criterion_group, criterion_main, Criterion};
use hermes_core::tools::{file_tools::FileReadTool, HermesTool, ToolContext};
use serde_json::json;
use std::fs::File;
use std::io::Write;
use tokio::runtime::Runtime;

fn bench_file_read(c: &mut Criterion) {
    let path = "large_test_file.txt";
    // Setup file
    let mut file = File::create(path).unwrap();
    let content = "A".repeat(10 * 1024 * 1024); // 10MB
    file.write_all(content.as_bytes()).unwrap();

    let rt = Runtime::new().unwrap();
    let tool = FileReadTool;
    let context = ToolContext::default();

    c.bench_function("file_read", |b| {
        b.iter(|| {
            rt.block_on(async {
                let args = json!({
                    "path": path,
                    "limit": 1
                });
                tool.execute(args, context.clone()).await;
            });
        });
    });

    std::fs::remove_file(path).unwrap();
}

criterion_group!(benches, bench_file_read);
criterion_main!(benches);
