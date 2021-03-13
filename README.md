# spmc_queue 

[![CI](https://github.com/ShayDamir/spmc_queue/actions/workflows/main.yml/badge.svg)](https://github.com/ShayDamir/spmc_queue/actions/workflows/main.yml)
[![codecov](https://codecov.io/gh/ShayDamir/spmc_queue/branch/main/graph/badge.svg?token=5QewrhH8Bt)](https://codecov.io/gh/ShayDamir/spmc_queue)
[![Built with cargo-make](https://sagiegurari.github.io/cargo-make/assets/badges/cargo-make.svg)](https://sagiegurari.github.io/cargo-make)
License: MIT OR  Apache-2.0

Single Producer Multiple Consumer Lock-free Bound queue written in Rust.

Can be used to efficiently implement work-stealing algorithms with polling.

The queue is bound and non-blocking. It can contain fixed number of elements,
and  the `push()` operation will fail if the queue is full, and `pop()` will
fail if the queue is empty.

Waiting can be implemented by external methods, or a polling method can be
used by checking for `is_full()` or `is_empty()`.

## Forward guarantees

`push()` operation is wait-free, i.e. it will be complete in a fixed amount of time.
`pop()` operation is lock-free, i.e. out of multiple concurrent `pop()` operations,
at least one will succeed. Others will retry the operations until they succeed.

## Example

```rust
# use std::thread;
use spmc_queue::SpmcQueue;
let mut queue = SpmcQueue::new();
let mut handles = Vec::new();

for t in 0..10 {
     let c = queue.to_consumer();
     handles.push(thread::spawn(move || {
         loop {
            match c.pop() {
             Some(element) => { println!("Consumer {} got {}", t, element); break; }
             None => { thread::yield_now(); }
            }
         }
     }));
}

for i in 0..10 {
    queue.push(i).unwrap();
}
for h in handles {
    h.join().unwrap();
}
```
