use core::sync::atomic::AtomicUsize;
use core::sync::atomic::Ordering::{AcqRel, Acquire, Release};
use std::sync::Arc;

struct SpmcQueueInner<T> {
    contents: Vec<T>,
    head: AtomicUsize,
    tail: AtomicUsize,
    capacity: usize,
}

pub struct SpmcQueueProducer<T> {
    inner: Arc<SpmcQueueInner<T>>,
}

#[derive(Clone)]
pub struct SpmcQueueConsumer<T> {
    inner: Arc<SpmcQueueInner<T>>,
}

unsafe impl<T: Send> Send for SpmcQueueProducer<T> {}
unsafe impl<T: Send> Send for SpmcQueueConsumer<T> {}
unsafe impl<T: Send> Sync for SpmcQueueConsumer<T> {}

pub fn spmc_new<T>(capacity: usize) -> (SpmcQueueProducer<T>, SpmcQueueConsumer<T>) {
    let inner = SpmcQueueInner {
        head: AtomicUsize::new(0),
        tail: AtomicUsize::new(0),
        capacity: capacity,
        contents: Vec::with_capacity(capacity),
    };
    assert!(capacity != 0);
    assert!(capacity & (capacity - 1) == 0);
    let arc = Arc::new(inner);
    let producer = SpmcQueueProducer::new(arc.clone());
    let consumer = SpmcQueueConsumer::new(arc);
    (producer, consumer)
}

impl<T> SpmcQueueProducer<T> {
    fn new(inner: Arc<SpmcQueueInner<T>>) -> Self {
        Self { inner: inner }
    }

    pub fn enqueue(&mut self, item: T) -> bool {
        let inner = &self.inner;
        let head = inner.head.load(Acquire);
        let tail = inner.tail.load(Acquire);
        let capacity = inner.capacity;
        if tail - head >= capacity {
            return false;
        }
        let idx = tail % capacity;
        let contents = inner.contents.as_ptr();
        unsafe {
            // memcpy(&inner.contents[idx], item, sizeof(item));
            // ownership is also transferred to the inner.contents
            core::ptr::write((contents as *mut T).add(idx), item);
        }
        inner.tail.store(tail + 1, Release);
        true
    }
}

impl<T> SpmcQueueConsumer<T> {
    fn new(inner: Arc<SpmcQueueInner<T>>) -> Self {
        Self { inner: inner }
    }
    pub fn dequeue(&self) -> Option<T> {
        let inner = &self.inner;
        let mut head = inner.head.load(Acquire);
        let capacity = inner.capacity;

        loop {
            let tail = inner.tail.load(Acquire);
            if tail == head {
                return None;
            }
            let idx = head % capacity;
            let contents = inner.contents.as_ptr();
            let result = unsafe { core::ptr::read(contents.add(idx)) };
            match inner
                .head
                .compare_exchange_weak(head, head + 1, AcqRel, Acquire)
            {
                Ok(_) => return Some(result),
                Err(x) => {
                    std::mem::forget(result);
                    head = x;
                }
            }
        }
    }
    pub fn size(&self) -> usize {
        let head = self.inner.head.load(Acquire);
        let tail = self.inner.tail.load(Acquire);
        tail - head
    }
}

#[cfg(test)]
mod tests {
    use crate::spmc_new;
    #[test]
    fn single_thread_usize() {
        let (mut p, c) = spmc_new(128);

        assert_eq!(c.size(), 0);
        assert!(p.enqueue(5));
        assert_eq!(c.size(), 1);
        assert_eq!(c.dequeue(), Some(5));
        assert_eq!(c.dequeue(), None);

        assert!(p.enqueue(1));
        assert!(p.enqueue(2));
        assert!(p.enqueue(3));
        assert_eq!(c.size(), 3);
        assert_eq!(c.dequeue(), Some(1));
        assert_eq!(c.dequeue(), Some(2));
        assert_eq!(c.dequeue(), Some(3));
        assert_eq!(c.dequeue(), None);

        let d = c.clone();
        assert!(p.enqueue(1));
        assert!(p.enqueue(2));
        assert_eq!(d.size(), c.size());
        assert_eq!(d.dequeue(), Some(1));
        assert_eq!(d.size(), c.size());
        assert_eq!(c.dequeue(), Some(2));
        assert_eq!(d.size(), c.size());
    }

    #[test]
    fn single_thread_box() {
        let (mut p, c) = spmc_new(128);
        assert!(p.enqueue(Box::new(5)));
        assert_eq!(c.dequeue(), Some(Box::new(5)));
        assert_eq!(c.dequeue(), None);
    }
    #[test]
    fn multi_thread_box() {
        let (mut p, c) = spmc_new(128);
        let mut consumers = Vec::new();
        for _i in 1..3 {
            let consumer = c.clone();
            consumers.push(std::thread::spawn(move || loop {
                match consumer.dequeue() {
                    Some(x) => {
                        assert_eq!(x, Box::new(5));
                        return;
                    }
                    None => continue,
                }
            }));
        }
        for _i in 1..3 {
            assert!(p.enqueue(Box::new(5)));
        }
        for handle in consumers {
            match handle.join() {
                Ok(_) => continue,
                Err(x) => panic!(x),
            }
        }
    }
}
