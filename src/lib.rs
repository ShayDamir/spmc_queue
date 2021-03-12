#![deny(warnings)]
#![warn(missing_docs)]

//! # SPMC Queue
//!
//! Single Producer, Multiple Consumer Bound Lock-free Queue. Can be used to
//! efficiently implement work-stealing algorithms with polling.
//!
//! The queue is bound and non-blocking. It can contain fixed number of elements,
//! and  the [`SpmcQueue::push()`] operation will fail if the queue is full, and [`SpmcQueue::pop()`] will
//! fail if the queue is empty.
//!
//! Waiting can be implemented by external methods, or a polling method can be
//! used by checking for [`SpmcQueue::is_full()`] or [`SpmcQueue::is_empty()`].
//!
//! ## Forward guarantees
//!
//! `push()` operation is wait-free, i.e. it will be complete in a fixed amount of time.
//! `pop()` operation is lock-free, i.e. out of multiple concurrent `pop()` operations,
//!  at least one will succeed. Others will retry the operations until they succeed.
//!
//! ## Example
//!
//! ```
//! # use std::thread;
//! use spmc_queue::SpmcQueue;
//! let mut queue = SpmcQueue::new();
//! let mut handles = Vec::new();
//!
//! for t in 0..10 {
//!     let c = queue.to_consumer();
//!     handles.push(thread::spawn(move || {
//!         loop {
//!            match c.pop() {
//!             Some(element) => { println!("Consumer {} got {}", t, element); break; }
//!             None => { thread::yield_now(); }
//!            }
//!         }
//!     }));
//! }
//!
//! for i in 0..10 {
//!     queue.push(i).unwrap();
//! }
//! for h in handles {
//!    h.join().unwrap();
//! }
//! ```

use core::fmt;
use core::sync::atomic::fence;
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release};
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

impl<T> SpmcQueueInner<T> {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            head: AtomicUsize::new(usize::MAX),
            tail: AtomicUsize::new(usize::MAX),
            contents: Vec::with_capacity(capacity),
        }
    }

    fn push(&self, item: T) -> Result<(), T> {
        let head = self.head.load(Acquire);
        let tail = self.tail.load(Acquire);
        if tail.wrapping_sub(head) == self.contents.capacity() {
            return Err(item);
        }
        let idx = tail % self.contents.capacity();
        let contents = self.contents.as_ptr() as *mut T;
        unsafe {
            core::ptr::write(contents.add(idx), item);
        }
        fence(Release);
        self.tail.store(tail.wrapping_add(1), Release);
        Ok(())
    }

    #[inline]
    fn load(&self, head: usize) -> T {
        let idx = head % self.contents.capacity();
        let contents = self.contents.as_ptr();
        fence(Acquire);
        unsafe { core::ptr::read(contents.add(idx)) }
    }

    #[inline]
    fn confirm(&self, head: usize, result: T) -> Result<T, usize> {
        match self
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

    fn pop(&self) -> Option<T> {
        let mut head = self.head.load(Acquire);

        loop {
            let tail = self.tail.load(Acquire);
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

    fn size(&self) -> usize {
        let head = self.head.load(Acquire);
        let tail = self.tail.load(Acquire);
        tail.wrapping_sub(head)
    }

    fn capacity(&self) -> usize {
        self.contents.capacity()
    }

    fn is_full(&self) -> bool {
        self.size() == self.capacity()
    }

    fn is_empty(&self) -> bool {
        self.size() == 0
    }
}

/// Queue producer structure used for pushing elements into the queue
///
/// There can be only one producer. It cannot be cloned, but it can be moved
/// to another thread.
pub struct SpmcQueue<T> {
    inner: Arc<SpmcQueueInner<T>>,
}

/// Queue consumer structure, used only for popping elements from the queue
///
/// This can be constructed from an existing SpmcQueue by calling
/// [`SpmcQueue::to_consumer()`], or by cloning an existing consumer.
///
/// ## Example:
/// ```
/// # use spmc_queue::SpmcQueue;
/// let mut queue = SpmcQueue::new();
/// let c1 = queue.to_consumer();
/// let c2 = c1.clone();
///
/// queue.push(1).unwrap();
/// queue.push(2).unwrap();
///
/// assert_eq!(c1.pop().unwrap(), 1);
/// assert_eq!(c2.pop().unwrap(), 2);
/// ```
pub struct SpmcQueueConsumer<T> {
    inner: Arc<SpmcQueueInner<T>>,
}

unsafe impl<T: Send> Send for SpmcQueue<T> {}
unsafe impl<T: Send> Send for SpmcQueueConsumer<T> {}
unsafe impl<T: Send> Sync for SpmcQueueConsumer<T> {}

impl<T> Default for SpmcQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> fmt::Display for SpmcQueue<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "SpmcQueue({} out of {})", self.size(), self.capacity())
    }
}

impl<T> fmt::Debug for SpmcQueue<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("SpmcQueue")
            .field("head", &self.inner.head.load(Relaxed))
            .field("tail", &self.inner.tail.load(Relaxed))
            .field("consumers", &(Arc::strong_count(&self.inner) - 1))
            .field("capacity", &self.capacity())
            .finish()
    }
}

impl<T> fmt::Display for SpmcQueueConsumer<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "SpmcQueueConsumer({} out of {})",
            self.size(),
            self.capacity()
        )
    }
}

impl<T> fmt::Debug for SpmcQueueConsumer<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("SpmcQueueConsumer")
            .field("head", &self.inner.head.load(Relaxed))
            .field("tail", &self.inner.tail.load(Relaxed))
            .field("capacity", &self.capacity())
            .finish()
    }
}

impl<T> From<SpmcQueue<T>> for SpmcQueueConsumer<T> {
    fn from(p: SpmcQueue<T>) -> Self {
        Self { inner: p.inner }
    }
}

impl<T> SpmcQueue<T> {
    /// Creates a new empty queue with capacity of 1024 elements.
    pub fn new() -> Self {
        Self::with_capacity(1024)
    }

    /// Creates a new empty queue with defined `capacity`
    ///
    /// The capacity must be a power of 2 for the queue to operate properly.
    ///
    /// ## Panics
    /// Panics if capacity is not a power of 2.
    pub fn with_capacity(capacity: usize) -> Self {
        assert!(capacity != 0);
        assert!(capacity & (capacity - 1) == 0);
        Self {
            inner: Arc::new(SpmcQueueInner::with_capacity(capacity)),
        }
    }

    /// Pushes an element to the tail of the queue
    ///
    /// On success, the item is moved into the queue. On failure, the item is
    /// returned back, wrapped in an `Err`.
    ///
    /// Returns `Ok(())` on success, and `Err(item)` on failure.
    ///
    /// This operation is wait-free.
    ///
    /// ## Example:
    /// ```
    /// # use spmc_queue::SpmcQueue;
    /// let mut queue = SpmcQueue::with_capacity(2);
    /// assert!(queue.push(1).is_ok());
    /// assert!(queue.push(2).is_ok());
    /// let result = queue.push(3);
    /// assert_eq!(result, Err(3));
    /// ```
    pub fn push(&mut self, item: T) -> Result<(), T> {
        self.inner.push(item)
    }

    /// Pops an element from the head of the queue
    ///
    /// This operation is lock-free.
    ///
    /// ## Example:
    ///
    /// ```
    /// # use spmc_queue::SpmcQueue;
    /// let mut queue = SpmcQueue::new();
    /// queue.push(1).unwrap();
    /// queue.push(2).unwrap();
    /// assert_eq!(queue.pop(), Some(1));
    /// assert_eq!(queue.pop(), Some(2));
    /// assert_eq!(queue.pop(), None);
    /// ```
    pub fn pop(&self) -> Option<T> {
        self.inner.pop()
    }

    /// Creates a new consumer for the Queue
    ///
    /// A consumer can only [`SpmcQueueConsumer::pop()`] from the queue, but it
    /// can be cloned and sent to different threads of execution.
    ///
    /// ## Example:
    /// ```
    /// # use spmc_queue::SpmcQueue;
    /// let mut queue = SpmcQueue::new();
    /// let c1 = queue.to_consumer();
    /// let c2 = c1.clone();
    ///
    /// queue.push(1).unwrap();
    /// queue.push(2).unwrap();
    ///
    /// assert_eq!(c1.pop(), Some(1));
    /// assert_eq!(c2.pop(), Some(2));
    /// assert_eq!(c1.pop(), None);
    /// ```
    pub fn to_consumer(&self) -> SpmcQueueConsumer<T> {
        SpmcQueueConsumer::new(self.inner.clone())
    }

    /// returns maximum number of elements the queue can contain
    pub fn capacity(&self) -> usize {
        self.inner.capacity()
    }

    /// returns current number of elements in the queue
    pub fn size(&self) -> usize {
        self.inner.size()
    }

    /// returns true if the queue is full, false otherwise
    pub fn is_full(&self) -> bool {
        self.inner.is_full()
    }

    /// returns true if the queue is empty, false otherwise
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
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

    /// Pops an element from the head of the queue
    ///
    /// This operation is lock-free.
    ///
    /// ## Example:
    ///
    /// ```
    /// # use spmc_queue::SpmcQueue;
    /// let mut queue = SpmcQueue::new();
    /// queue.push(1).unwrap();
    /// queue.push(2).unwrap();
    ///
    /// let consumer = queue.to_consumer();
    /// assert_eq!(consumer.pop(), Some(1));
    /// assert_eq!(consumer.pop(), Some(2));
    /// assert_eq!(consumer.pop(), None);
    /// ```
    pub fn pop(&self) -> Option<T> {
        self.inner.pop()
    }

    /// returns current number of elements in the queue
    pub fn size(&self) -> usize {
        self.inner.size()
    }

    /// returns maximum number of elements the queue can contain
    pub fn capacity(&self) -> usize {
        self.inner.capacity()
    }

    /// returns true if the queue is empty, false otherwise
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use crate::{SpmcQueue, SpmcQueueConsumer};
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
    fn example_test() {
        let mut p = SpmcQueue::default();
        let c = p.to_consumer();
        for i in 0..10 {
            p.push(i).unwrap();
        }
        let mut r = 0;
        while let Some(i) = c.pop() {
            assert_eq!(r, i);
            r += 1;
        }
    }

    #[test]
    fn single_thread_usize() {
        let n = 128;
        let mut p = SpmcQueue::with_capacity(128);
        let c = p.to_consumer();

        assert_eq!(c.size(), 0);
        assert!(c.is_empty());
        assert!(p.is_empty());
        assert!(!p.is_full());

        p.push(5).unwrap();

        assert_eq!(c.size(), 1);
        assert!(!c.is_empty());
        assert!(!p.is_empty());
        assert!(!p.is_full());

        assert_eq!(c.pop(), Some(5));
        assert_eq!(c.size(), 0);
        assert!(c.pop().is_none());

        for i in 0..n {
            p.push(i).unwrap();
            assert_eq!(c.size(), i + 1);
        }

        let d = c.clone();

        for i in 0..n {
            assert_eq!(c.size(), n - i);
            assert_eq!(d.size(), n - i);
            if i < n / 2 {
                assert_eq!(c.pop(), Some(i));
            } else {
                assert_eq!(d.pop(), Some(i));
            }
        }
        assert!(c.pop().is_none());
        assert!(d.pop().is_none());
        assert_eq!(c.capacity(), n);
        assert_eq!(d.capacity(), n);
        assert_eq!(p.capacity(), n);
    }

    #[test]
    fn single_thread_box() {
        let mut p = SpmcQueue::new();
        let c = p.to_consumer();
        p.push(Box::new(5)).unwrap();
        assert_eq!(c.pop(), Some(Box::new(5)));
        assert!(c.pop().is_none());
        let c2 = c.clone();
        p.push(Box::new(42)).unwrap();
        assert_eq!(c2.pop(), Some(Box::new(42)));
        assert!(c.pop().is_none());
    }

    #[test]
    fn no_consumers_test() {
        let ctr = Arc::new(AtomicUsize::new(0));
        let mut p = SpmcQueue::default();
        assert!(p.is_empty());
        assert!(!p.is_full());
        for i in 0..p.capacity() {
            assert!(!p.is_full());
            p.push(DropTest::new(&ctr)).unwrap();
            assert_eq!(ctr.load(Relaxed), i + 1);
            assert!(!p.is_empty());
        }
        assert!(p.push(DropTest::new(&ctr)).is_err());
        assert!(!p.is_empty());
        assert!(p.is_full());
        core::mem::drop(p);
        assert_eq!(ctr.load(Relaxed), 0);
    }

    #[test]
    fn fmt_test() {
        let p: SpmcQueue<usize> = SpmcQueue::with_capacity(64);
        let c = p.to_consumer();
        println!("Display: {} {}", p, c);
        println!("Debug: {:?} {:?}", p, c);
    }

    #[test]
    fn test_to_consumer() {
        let p: SpmcQueue<usize> = SpmcQueue::with_capacity(64);
        let c1 = p.to_consumer();
        let c2 = c1.clone();
        assert_eq!(c1.capacity(), p.capacity());
        assert_eq!(c1.capacity(), c2.capacity());
        /* this consumes the queue */
        let c3 = SpmcQueueConsumer::from(p);
        assert_eq!(c1.capacity(), c3.capacity());
    }

    #[test]
    fn single_thread_drop_test() {
        let ctr = Arc::new(AtomicUsize::new(0));
        let mut p = SpmcQueue::with_capacity(128);
        let c = p.to_consumer();
        p.push(DropTest::new(&ctr)).unwrap();
        c.pop().unwrap();
        assert!(c.pop().is_none());
        core::mem::drop(p);
        core::mem::drop(c);
        assert_eq!(ctr.load(Relaxed), 0);
    }

    #[test]
    fn race_condition_check() {
        let mut p = SpmcQueue::with_capacity(128);
        let c1 = p.to_consumer();
        let c2 = c1.clone();

        let ctr = Arc::new(AtomicUsize::new(0));
        p.push(DropTest::new(&ctr)).unwrap();
        let head = p.inner.head.load(Relaxed);

        /* Simulate race condition, when 2 different
         * consumers load the same value, then try to confirm it.
         * Only one should succeed */
        let r1 = c1.inner.load(head);
        let r2 = c2.inner.load(head);

        let con1 = c1.inner.confirm(head, r1);
        let con2 = c2.inner.confirm(head, r2);

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
            let consumer = p.to_consumer();
            let r_ctr = recv_ctr.clone();
            consumers.push(thread::spawn(move || {
                while r_ctr.load(Relaxed) < n {
                    match consumer.pop() {
                        Some(_) => {
                            r_ctr.fetch_add(1, Relaxed);
                        }
                        None => {
                            thread::yield_now();
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
                match p.push(DropTest::new(&producer_ctr)) {
                    Err(_) => thread::yield_now(),
                    Ok(()) => i += 1,
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
