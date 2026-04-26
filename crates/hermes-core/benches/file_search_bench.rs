use criterion::{black_box, criterion_group, criterion_main, Criterion};
use hermes_core::tools::{FileSearchTool, HermesTool, ToolContext};
use serde_json::json;
use std::io::Write;
use tempfile::NamedTempFile;

fn file_search_benchmark(c: &mut Criterion) {
    let mut temp_file = NamedTempFile::new().unwrap();
    // Write ~10MB of dummy data
    let dummy_line =
        "This is a dummy log line that contains some information but not the target.\n";
    let target_line = "This is the TARGET line we are searching for near the end.\n";

    for _ in 0..150_000 {
        temp_file.write_all(dummy_line.as_bytes()).unwrap();
    }
    temp_file.write_all(target_line.as_bytes()).unwrap();
    for _ in 0..10 {
        temp_file.write_all(dummy_line.as_bytes()).unwrap();
    }

    let path = temp_file.path().to_string_lossy().to_string();

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let tool = FileSearchTool;
    let context = ToolContext::default();

    let args_case_insensitive = json!({
        "path": path,
        "pattern": "TARGET",
        "caseSensitive": false,
        "maxResults": 10
    });

    let args_case_sensitive = json!({
        "path": path,
        "pattern": "TARGET",
        "caseSensitive": true,
        "maxResults": 10
    });

    let mut group = c.benchmark_group("file_search");

    group.bench_function("case_insensitive", |b| {
        b.to_async(&runtime).iter(|| async {
            let result = tool
                .execute(black_box(args_case_insensitive.clone()), context.clone())
                .await;
            black_box(result);
        })
    });

    group.bench_function("case_sensitive", |b| {
        b.to_async(&runtime).iter(|| async {
            let result = tool
                .execute(black_box(args_case_sensitive.clone()), context.clone())
                .await;
            black_box(result);
        })
    });

    group.finish();
}

criterion_group!(benches, file_search_benchmark);
criterion_main!(benches);
