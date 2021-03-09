use criterion::{black_box, criterion_group, criterion_main, Criterion};
use spmc::spmc_new;

fn single_thread_bench(c: &mut Criterion) {
    let (mut producer, consumer) = spmc_new(128);
    c.bench_function("single", |b| {
        b.iter(|| {
            producer.enqueue(black_box(20));
            consumer.dequeue().unwrap();
        })
    });
}

criterion_group!(benches, single_thread_bench);
criterion_main!(benches);
