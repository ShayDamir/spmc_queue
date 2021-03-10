use core::sync::atomic::fence;
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::Ordering::{AcqRel, Acquire, Release};
use std::sync::Arc;

struct SpmcQueueInner<T> {
    contents: Vec<T>,
    head: AtomicUsize,
    tail: AtomicUsize,
}

impl<T> Drop for SpmcQueueInner<T> {
    fn drop(&mut self) {
        let mut head = self.head.load(Acquire);
        let tail = self.tail.load(Acquire);
        let contents = self.contents.as_mut_ptr();
        while head != tail {
            let idx = head % self.contents.capacity();
            unsafe { core::ptr::drop_in_place(contents.add(idx)) };
            head = head.wrapping_add(1);
        }
    }
}

pub struct SpmcQueue<T> {
    inner: Arc<SpmcQueueInner<T>>,
}

pub struct SpmcQueueConsumer<T> {
    inner: Arc<SpmcQueueInner<T>>,
}

unsafe impl<T: Send> Send for SpmcQueue<T> {}
unsafe impl<T: Send> Send for SpmcQueueConsumer<T> {}
unsafe impl<T: Send> Sync for SpmcQueueConsumer<T> {}

impl<T> SpmcQueue<T> {
    pub fn with_capacity(capacity: usize) -> Self {
        let inner = Arc::new(SpmcQueueInner {
            head: AtomicUsize::new(usize::MAX),
            tail: AtomicUsize::new(usize::MAX),
            contents: Vec::with_capacity(capacity),
        });
        assert!(capacity != 0);
        assert!(capacity & (capacity - 1) == 0);
        Self { inner }
    }

    pub fn enqueue(&mut self, item: T) -> bool {
        let inner = &self.inner;
        let head = inner.head.load(Acquire);
        let tail = inner.tail.load(Acquire);
        if tail.wrapping_sub(head) == inner.contents.capacity() {
            return false;
        }
        let idx = tail % inner.contents.capacity();
        let contents = inner.contents.as_ptr() as *mut T;
        unsafe {
            core::ptr::write(contents.add(idx), item);
        }
        fence(Release);
        inner.tail.store(tail.wrapping_add(1), Release);
        true
    }

    pub fn add_consumer(&self) -> SpmcQueueConsumer<T> {
        SpmcQueueConsumer::new(self.inner.clone())
    }

    pub fn capacity(&self) -> usize {
        self.inner.contents.capacity()
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

    #[inline]
    fn load(&self, head: usize) -> T {
        let idx = head % self.inner.contents.capacity();
        let contents = self.inner.contents.as_ptr();
        fence(Acquire);
        unsafe { core::ptr::read(contents.add(idx)) }
    }

    #[inline]
    fn confirm(&self, head: usize, result: T) -> Result<T, usize> {
        match self
            .inner
            .head
            .compare_exchange(head, head.wrapping_add(1), AcqRel, Acquire)
        {
            Ok(_) => Ok(result),
            Err(x) => {
                core::mem::forget(result);
                Err(x)
            }
        }
    }

    pub fn dequeue(&self) -> Option<T> {
        let inner = &self.inner;
        let mut head = inner.head.load(Acquire);

        loop {
            let tail = inner.tail.load(Acquire);
            if tail == head {
                return None;
            }
            let result = self.load(head);
            match self.confirm(head, result) {
                Ok(x) => return Some(x),
                Err(h) => head = h,
            }
        }
    }

    pub fn size(&self) -> usize {
        let head = self.inner.head.load(Acquire);
        let tail = self.inner.tail.load(Acquire);
        tail.wrapping_sub(head)
    }

    pub fn capacity(&self) -> usize {
        self.inner.contents.capacity()
    }
}

#[cfg(test)]
mod tests {
    use crate::SpmcQueue;
    use core::sync::atomic::AtomicUsize;
    use core::sync::atomic::Ordering::Relaxed;
    use std::sync::Arc;
    use std::thread;

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
        let mut p = SpmcQueue::with_capacity(128);
        let c = p.add_consumer();

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
        assert_eq!(c.capacity(), n);
        assert_eq!(d.capacity(), n);
        assert_eq!(p.capacity(), n);
    }

    #[test]
    fn single_thread_box() {
        let mut p = SpmcQueue::with_capacity(128);
        let c = p.add_consumer();
        assert!(p.enqueue(Box::new(5)));
        assert_eq!(c.dequeue(), Some(Box::new(5)));
        assert!(c.dequeue().is_none());
        let c2 = c.clone();
        assert!(p.enqueue(Box::new(42)));
        assert_eq!(c2.dequeue(), Some(Box::new(42)));
        assert!(c.dequeue().is_none());
    }

    #[test]
    fn no_consumers_test() {
        let ctr = Arc::new(AtomicUsize::new(0));
        let mut p = SpmcQueue::with_capacity(64);
        for i in 0..p.capacity() {
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
        let mut p = SpmcQueue::with_capacity(128);
        let c = p.add_consumer();
        assert!(p.enqueue(DropTest::new(&ctr)));
        c.dequeue().unwrap();
        assert!(c.dequeue().is_none());
        core::mem::drop(p);
        core::mem::drop(c);
        assert_eq!(ctr.load(Relaxed), 0);
    }

    #[test]
    fn race_condition_check() {
        let mut p = SpmcQueue::with_capacity(128);
        let c1 = p.add_consumer();
        let c2 = c1.clone();

        let ctr = Arc::new(AtomicUsize::new(0));
        assert!(p.enqueue(DropTest::new(&ctr)));
        let head = p.inner.head.load(Relaxed);

        /* Simulate race condition, when 2 different
         * consumers load the same value, then try to confirm it.
         * Only one should succeed */
        let r1 = c1.load(head);
        let r2 = c2.load(head);

        let con1 = c1.confirm(head, r1);
        let con2 = c2.confirm(head, r2);

        /* only first confirmation succeeds */
        assert!(con1.is_ok());
        con1.unwrap();
        assert_eq!(con2.err(), Some(head.wrapping_add(1)));

        assert_eq!(ctr.load(Relaxed), 0);
    }

    #[test]
    fn multi_thread_box() {
        let recv_ctr = Arc::new(AtomicUsize::new(0));
        let n = 1024 * 1024;
        let nthreads = 3;
        let mut p = SpmcQueue::with_capacity(128);
        let mut consumers = Vec::new();
        for _ in 0..nthreads {
            let consumer = p.add_consumer();
            let r_ctr = recv_ctr.clone();
            consumers.push(thread::spawn(move || {
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
        let drop_ctr = Arc::new(AtomicUsize::new(0));
        let producer_ctr = drop_ctr.clone();
        let producer_handle = thread::spawn(move || {
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
