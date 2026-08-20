use criterion::{criterion_group, criterion_main, Criterion};
use kerux_core::tools::{file_tools::FileReadTool, KeruxTool, ToolContext};
use serde_json::json;
use std::fs::File;
use std::io::Write;
use tokio::runtime::Runtime;

fn bench_file_read(c: &mut Criterion) {
    let path =
        std::env::temp_dir().join(format!("kerux-file-read-bench-{}.txt", std::process::id()));
    let mut file = File::create(&path).unwrap();
    file.write_all("A".repeat(10 * 1024 * 1024).as_bytes())
        .unwrap();
    let path_arg = path.to_string_lossy().into_owned();

    let rt = Runtime::new().unwrap();
    let tool = FileReadTool;
    let context = ToolContext::default();

    c.bench_function("file_read", |b| {
        b.to_async(&rt).iter(|| async {
            let args = json!({
                "path": path_arg,
                "limit": 1
            });
            tool.execute(args, context.clone()).await;
        });
    });

    let _ = std::fs::remove_file(path);
}

criterion_group!(benches, bench_file_read);
criterion_main!(benches);
