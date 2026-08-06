use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use tokio::runtime::Runtime;

fn create_temp_file(size: usize) -> PathBuf {
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "bench_patch_{}.txt",
        std::time::UNIX_EPOCH.elapsed().unwrap().as_nanos()
    ));
    let mut file = File::create(&path).unwrap();
    let content = "hello world\n".repeat(size / 12);
    file.write_all(content.as_bytes()).unwrap();
    path
}

fn bench_sync_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("sync_write");
    let path = create_temp_file(1024 * 1024); // 1MB
    let content = std::fs::read_to_string(&path).unwrap();

    group.bench_function("std::fs::write", |b| {
        b.iter(|| {
            std::fs::write(&path, black_box(&content)).unwrap();
        })
    });

    group.finish();
    std::fs::remove_file(path).unwrap();
}

fn bench_async_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("async_write");
    let path = create_temp_file(1024 * 1024); // 1MB
    let content = std::fs::read_to_string(&path).unwrap();
    let rt = Runtime::new().unwrap();

    group.bench_function("tokio::fs::write", |b| {
        b.to_async(&rt).iter(|| async {
            tokio::fs::write(&path, black_box(&content)).await.unwrap();
        })
    });

    group.finish();
    std::fs::remove_file(path).unwrap();
}

criterion_group!(benches, bench_sync_write, bench_async_write);
criterion_main!(benches);
