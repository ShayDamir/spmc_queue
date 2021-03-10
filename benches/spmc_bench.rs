use criterion::{black_box, criterion_group, criterion_main, Criterion};
use spmc::SpmcQueue;

fn single_thread_bench(c: &mut Criterion) {
    let mut producer = SpmcQueue::with_capacity(128);
    let consumer = producer.add_consumer();
    c.bench_function("single", |b| {
        b.iter(|| {
            producer.enqueue(black_box(20));
            consumer.dequeue().unwrap();
        })
    });
}

criterion_group!(benches, single_thread_bench);
criterion_main!(benches);
