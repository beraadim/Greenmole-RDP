//! Nonblocking FIFO admission with both message and byte limits. The byte lease
//! remains held while a job runs, including blocked storage I/O.

use std::sync::{mpsc, Arc};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(crate) struct WorkQueue<T> {
    sender: mpsc::SyncSender<WorkItem<T>>,
    budget: Arc<Semaphore>,
}

pub(crate) struct WorkItem<T> {
    pub value: T,
    _bytes: OwnedSemaphorePermit,
}

impl<T> WorkQueue<T> {
    pub fn new(capacity: usize, bytes: usize) -> (Self, mpsc::Receiver<WorkItem<T>>) {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        (
            Self {
                sender,
                budget: Arc::new(Semaphore::new(bytes)),
            },
            receiver,
        )
    }

    /// Reserve bytes before copying a borrowed network payload. Byte-budget
    /// rejection does not clone; a full message queue may reject after cloning,
    /// but that temporary copy still holds its byte-budget permit.
    pub fn try_push(&self, bytes: usize, make: impl FnOnce() -> T) -> bool {
        let permit = u32::try_from(bytes)
            .ok()
            .and_then(|bytes| self.budget.clone().try_acquire_many_owned(bytes).ok());
        let Some(permit) = permit else {
            return false;
        };
        self.sender
            .try_send(WorkItem {
                value: make(),
                _bytes: permit,
            })
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stalled_job_retains_byte_budget_and_rejected_payload_is_not_copied() {
        let (queue, rx) = WorkQueue::new(4, 100);
        assert!(queue.try_push(80, || 1));
        let running = rx.recv().unwrap();
        assert!(!queue.try_push(30, || panic!("must reserve before copying")));
        assert!(queue.try_push(20, || 2));
        drop(running);
        assert!(queue.try_push(80, || 3));
        assert_eq!(rx.recv().unwrap().value, 2);
        assert_eq!(rx.recv().unwrap().value, 3);
    }

    #[test]
    fn full_or_closed_fifo_fails_immediately_without_losing_existing_order() {
        let (queue, rx) = WorkQueue::new(2, 100);
        assert!(queue.try_push(10, || 1));
        assert!(queue.try_push(10, || 2));
        assert!(!queue.try_push(10, || 3));
        assert_eq!(rx.recv().unwrap().value, 1);
        assert_eq!(rx.recv().unwrap().value, 2);
        drop(rx);
        assert!(!queue.try_push(10, || 4));
    }
}
