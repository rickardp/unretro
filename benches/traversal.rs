//! Benchmarks for core archive traversal operations.
//!
//! Run with: `cargo bench --bench traversal`

// criterion's macros expand to items without doc comments.
#![allow(missing_docs)]

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use unretro::{EntryType, Loader, VisitAction};

/// Build a ZIP archive in memory with `n` entries of `entry_size` bytes each.
fn make_zip(n: usize, entry_size: usize) -> Vec<u8> {
    use std::io::Write;
    let buf = Vec::new();
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(buf));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let payload = vec![0xABu8; entry_size];
    for i in 0..n {
        writer
            .start_file(format!("file_{i:04}.bin"), options)
            .unwrap();
        writer.write_all(&payload).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

/// Build a TAR archive in memory with `n` entries.
fn make_tar(n: usize, entry_size: usize) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    let payload = vec![0xCDu8; entry_size];
    for i in 0..n {
        let mut header = tar::Header::new_gnu();
        header.set_path(format!("entry_{i:04}.dat")).unwrap();
        header.set_size(entry_size as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, &payload[..]).unwrap();
    }
    builder.into_inner().unwrap()
}

fn bench_zip_traversal(c: &mut Criterion) {
    let small = make_zip(10, 64);
    let medium = make_zip(100, 1024);
    let large = make_zip(1000, 256);

    let mut group = c.benchmark_group("zip_traversal");

    group.bench_function("10_entries_64B", |b| {
        b.iter(|| {
            let mut count = 0usize;
            Loader::from_bytes(black_box(small.clone()), "bench.zip")
                .visit(EntryType::Leaves, |_entry| {
                    count += 1;
                    Ok(VisitAction::Continue)
                })
                .unwrap();
            count
        });
    });

    group.bench_function("100_entries_1K", |b| {
        b.iter(|| {
            let mut count = 0usize;
            Loader::from_bytes(black_box(medium.clone()), "bench.zip")
                .visit(EntryType::Leaves, |_entry| {
                    count += 1;
                    Ok(VisitAction::Continue)
                })
                .unwrap();
            count
        });
    });

    group.bench_function("1000_entries_256B", |b| {
        b.iter(|| {
            let mut count = 0usize;
            Loader::from_bytes(black_box(large.clone()), "bench.zip")
                .visit(EntryType::Leaves, |_entry| {
                    count += 1;
                    Ok(VisitAction::Continue)
                })
                .unwrap();
            count
        });
    });

    group.finish();
}

fn bench_tar_traversal(c: &mut Criterion) {
    let small = make_tar(10, 64);
    let medium = make_tar(100, 1024);
    let large = make_tar(1000, 256);

    let mut group = c.benchmark_group("tar_traversal");

    group.bench_function("10_entries_64B", |b| {
        b.iter(|| {
            let mut count = 0usize;
            Loader::from_bytes(black_box(small.clone()), "bench.tar")
                .visit(EntryType::Leaves, |_entry| {
                    count += 1;
                    Ok(VisitAction::Continue)
                })
                .unwrap();
            count
        });
    });

    group.bench_function("100_entries_1K", |b| {
        b.iter(|| {
            let mut count = 0usize;
            Loader::from_bytes(black_box(medium.clone()), "bench.tar")
                .visit(EntryType::Leaves, |_entry| {
                    count += 1;
                    Ok(VisitAction::Continue)
                })
                .unwrap();
            count
        });
    });

    group.bench_function("1000_entries_256B", |b| {
        b.iter(|| {
            let mut count = 0usize;
            Loader::from_bytes(black_box(large.clone()), "bench.tar")
                .visit(EntryType::Leaves, |_entry| {
                    count += 1;
                    Ok(VisitAction::Continue)
                })
                .unwrap();
            count
        });
    });

    group.finish();
}

fn bench_format_detection(c: &mut Criterion) {
    let zip = make_zip(1, 64);
    let tar = make_tar(1, 64);
    let random: Vec<u8> = (0..1024).map(|i| (i * 37 % 256) as u8).collect();

    let mut group = c.benchmark_group("format_detection");

    group.bench_function("zip", |b| {
        b.iter(|| unretro::detect_format(black_box("test.zip"), Some(black_box(&zip))));
    });

    group.bench_function("tar", |b| {
        b.iter(|| unretro::detect_format(black_box("test.tar"), Some(black_box(&tar))));
    });

    group.bench_function("unknown", |b| {
        b.iter(|| unretro::detect_format(black_box("test.bin"), Some(black_box(&random))));
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_zip_traversal,
    bench_tar_traversal,
    bench_format_detection,
);
criterion_main!(benches);
