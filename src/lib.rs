use core::sync::atomic::fence;
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::Ordering::{AcqRel, Acquire, Release};
use std::sync::Arc;

struct SpmcQueueInner<T> {
    contents: Vec<T>,
    head: AtomicUsize,
    tail: AtomicUsize,
    capacity: usize,
}

impl<T> Drop for SpmcQueueInner<T> {
    fn drop(&mut self) {
        let mut head = self.head.load(Acquire);
        let tail = self.tail.load(Acquire);
        let contents = self.contents.as_mut_ptr();
        while head != tail {
            let idx = head % self.capacity;
            unsafe { core::ptr::drop_in_place(contents.add(idx)) };
            head = head.wrapping_add(1);
        }
    }
}

pub struct SpmcQueueProducer<T> {
    inner: Arc<SpmcQueueInner<T>>,
}

pub struct SpmcQueueConsumer<T> {
    inner: Arc<SpmcQueueInner<T>>,
}

unsafe impl<T: Send> Send for SpmcQueueProducer<T> {}
unsafe impl<T: Send> Send for SpmcQueueConsumer<T> {}
unsafe impl<T: Send> Sync for SpmcQueueConsumer<T> {}

pub fn spmc_new<T>(capacity: usize) -> (SpmcQueueProducer<T>, SpmcQueueConsumer<T>) {
    let inner = SpmcQueueInner {
        head: AtomicUsize::new(usize::MAX),
        tail: AtomicUsize::new(usize::MAX),
        capacity,
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
        Self { inner }
    }

    pub fn enqueue(&mut self, item: T) -> bool {
        let inner = &self.inner;
        let head = inner.head.load(Acquire);
        let tail = inner.tail.load(Acquire);
        let capacity = inner.capacity;
        if tail.wrapping_sub(head) == capacity {
            return false;
        }
        let idx = tail % capacity;
        let contents = inner.contents.as_ptr() as *mut T;
        unsafe {
            core::ptr::write(contents.add(idx), item);
        }
        fence(Release);
        inner.tail.store(tail.wrapping_add(1), Release);
        true
    }
    pub fn to_consumer(&self) -> SpmcQueueConsumer<T> {
        SpmcQueueConsumer::new(self.inner.clone())
    }
}

impl<T> Clone for SpmcQueueConsumer<T> {
    fn clone(&self) -> SpmcQueueConsumer<T> {
        SpmcQueueConsumer::new(self.inner.clone())
    }
}

impl<T> SpmcQueueConsumer<T> {
    fn new(inner: Arc<SpmcQueueInner<T>>) -> Self {
        Self { inner }
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
            fence(Acquire);
            let result = unsafe { core::ptr::read(contents.add(idx)) };
            match inner
                .head
                .compare_exchange_weak(head, head.wrapping_add(1), AcqRel, Acquire)
            {
                Ok(_) => return Some(result),
                Err(x) => {
                    core::mem::forget(result);
                    head = x;
                }
            }
        }
    }
    pub fn size(&self) -> usize {
        let head = self.inner.head.load(Acquire);
        let tail = self.inner.tail.load(Acquire);
        tail.wrapping_sub(head)
    }
}

#[cfg(test)]
mod tests {
    use crate::spmc_new;
    use core::sync::atomic::AtomicUsize;
    use core::sync::atomic::Ordering::Relaxed;
    use std::sync::Arc;

    #[derive(Debug)]
    struct DropTest(Arc<AtomicUsize>);
    impl Drop for DropTest {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Relaxed);
        }
    }
    impl DropTest {
        fn new(inner: &Arc<AtomicUsize>) -> Self {
            let i = inner.clone();
            i.fetch_add(1, Relaxed);
            DropTest(i)
        }
    }
    #[test]
    fn single_thread_usize() {
        let n = 128;
        let (mut p, c) = spmc_new(128);

        assert_eq!(c.size(), 0);
        assert!(p.enqueue(5));
        assert_eq!(c.size(), 1);
        assert_eq!(c.dequeue(), Some(5));
        assert_eq!(c.size(), 0);
        assert!(c.dequeue().is_none());

        for i in 0..n {
            assert!(p.enqueue(i));
            assert_eq!(c.size(), i + 1);
        }

        let d = c.clone();

        for i in 0..n {
            assert_eq!(c.size(), n - i);
            assert_eq!(d.size(), n - i);
            if i < n / 2 {
                assert_eq!(c.dequeue(), Some(i));
            } else {
                assert_eq!(d.dequeue(), Some(i));
            }
        }
        assert!(c.dequeue().is_none());
        assert!(d.dequeue().is_none());
    }

    #[test]
    fn single_thread_box() {
        let (mut p, c) = spmc_new(128);
        assert!(p.enqueue(Box::new(5)));
        assert_eq!(c.dequeue(), Some(Box::new(5)));
        assert!(c.dequeue().is_none());
        let c2 = p.to_consumer();
        assert!(p.enqueue(Box::new(42)));
        assert_eq!(c2.dequeue(), Some(Box::new(42)));
        assert!(c.dequeue().is_none());
    }

    #[test]
    fn no_consumers_test() {
        let ctr = Arc::new(AtomicUsize::new(0));
        let (mut p, _) = spmc_new(64);
        for i in 0..64 {
            assert!(p.enqueue(DropTest::new(&ctr)));
            assert_eq!(ctr.load(Relaxed), i + 1);
        }
        assert!(!p.enqueue(DropTest::new(&ctr)));
        core::mem::drop(p);
        assert_eq!(ctr.load(Relaxed), 0);
    }

    #[test]
    fn single_thread_drop_test() {
        let ctr = Arc::new(AtomicUsize::new(0));
        let (mut p, c) = spmc_new(128);
        assert!(p.enqueue(DropTest::new(&ctr)));
        c.dequeue().unwrap();
        assert!(c.dequeue().is_none());
        core::mem::drop(p);
        core::mem::drop(c);
        assert_eq!(ctr.load(Relaxed), 0);
    }

    #[test]
    fn multi_thread_box() {
        let recv_ctr = Arc::new(AtomicUsize::new(0));
        let n = 1024 * 1024;
        let nthreads = 3;
        let (mut p, c) = spmc_new(128);
        let mut consumers = Vec::new();
        for _ in 0..nthreads {
            let consumer = c.clone();
            let r_ctr = recv_ctr.clone();
            consumers.push(std::thread::spawn(move || {
                while r_ctr.load(Relaxed) < n {
                    match consumer.dequeue() {
                        Some(_) => {
                            r_ctr.fetch_add(1, Relaxed);
                        }
                        None => {
                            std::thread::yield_now();
                        }
                    }
                }
            }));
        }
        core::mem::drop(c);
        let drop_ctr = Arc::new(AtomicUsize::new(0));
        let producer_ctr = drop_ctr.clone();
        let producer_handle = std::thread::spawn(move || {
            let mut i = 0;
            while i < n {
                match p.enqueue(DropTest::new(&producer_ctr)) {
                    false => std::thread::yield_now(),
                    true => i += 1,
                };
            }
        });
        for handle in consumers.into_iter() {
            handle.join().unwrap();
        }
        producer_handle.join().unwrap();
        assert_eq!(drop_ctr.load(Relaxed), 0);
        assert_eq!(recv_ctr.load(Relaxed), n);
    }
}
