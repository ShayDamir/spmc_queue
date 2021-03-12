use criterion::{black_box, criterion_group, criterion_main, Criterion};
use spmc_queue::SpmcQueue;

fn single_thread_bench(c: &mut Criterion) {
    let mut producer = SpmcQueue::with_capacity(128);
    let consumer = producer.to_consumer();
    c.bench_function("single", |b| {
        b.iter(|| {
            producer.push(black_box(20)).unwrap();
            consumer.pop().unwrap();
        })
    });
}

criterion_group!(benches, single_thread_bench);
criterion_main!(benches);
