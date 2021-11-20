//! Cancelable task implementation.
//! This module contains methods for spawning cancelable tasks and interfaces to manage them.
//!
//! Look at the following example:
//!
//! ```
//! use coachman as cm;
//! use coachman::{try_await, Canceled, Completed, TaskError};
//!
//! async fn inner_func(i: usize, duration: u64) {
//!     match try_await!(tokio::time::sleep(std::time::Duration::from_secs(duration))) {
//!         Canceled => println!("task#{} inner canceled", i),
//!         Completed(_) => println!("task#{} inner completed", i),
//!     }
//! }
//!
//! async fn outer_func(i: usize, duration: u64) {
//!     match try_await!(inner_func(i, duration)) {
//!         Canceled => println!("task#{} outer canceled", i),
//!         Completed(_) => println!("task#{} outer completed", i),
//!     }
//! }
//!
//! #[tokio::main(flavor = "current_thread")]
//! async fn main() {
//!     let mut task_handles = Vec::new();
//!     for i in 0..5 {
//!         let duration = i as u64;
//!         task_handles.push(cm::spawn(outer_func(i, duration)));
//!     }
//!
//!     let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
//!     for (i, mut handle) in task_handles.into_iter().enumerate() {
//!         if tokio::time::timeout_at(deadline, &mut handle).await.is_ok() {
//!             println!("task-{} completed", i);
//!         } else {
//!             handle.cancel();
//!             match handle.await {
//!                 Result::Err(TaskError::Canceled) => println!("task-{} canceled", i),
//!                 Result::Err(TaskError::Aborted) => println!("task-{} aborted", i),
//!                 Result::Err(TaskError::Panicked(_)) => println!("task-{} panicked", i),
//!                 Result::Ok(_) => unreachable!(),
//!             }
//!         }
//!     }
//! }
//! ```

use std::any::Any;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::macros::support::Future;
use tokio::sync::watch;
use tokio::task;

tokio::task_local! {
    pub static TASK_CANCEL_EVENT: watch::Receiver<bool>;
}

/// Returns `true` if current task is canceled.
pub fn is_task_canceled() -> bool {
    *TASK_CANCEL_EVENT.with(|event| event.clone()).borrow()
}

/// Spawns a new cancelable task returning [`TaskHandle`] for it.
pub fn spawn<F>(future: F) -> TaskHandle<F::Output>
where
    F: Future<Output = ()> + Send + 'static,
{
    return spawn_inner(future, async {});
}

pub(crate) fn spawn_inner<F, C>(future: F, complete_callback_future: C) -> TaskHandle<F::Output>
where
    F: Future<Output = ()> + Send + 'static,
    C: Future<Output = ()> + Send + 'static,
{
    let (cancel_event_sender, cancel_event_receiver) = watch::channel(false);

    let handle = tokio::spawn(TASK_CANCEL_EVENT.scope(
        cancel_event_receiver,
        Task {
            future: Box::pin(future),
            complete_callback_future: Box::pin(complete_callback_future),
        },
    ));

    return TaskHandle {
        handle,
        cancel_event_sender,
        canceled: false,
    };
}

struct Task<F, C>
where
    F: Future<Output = ()> + Send + 'static,
    C: Future<Output = ()> + Send + 'static,
{
    future: Pin<Box<F>>,
    complete_callback_future: Pin<Box<C>>,
}

impl<F, C> Future for Task<F, C>
where
    F: Future<Output = ()> + Send + 'static,
    C: Future<Output = ()> + Send + 'static,
{
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.future.as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(future_result) => match self.complete_callback_future.as_mut().poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(_) => Poll::Ready(future_result),
            },
        }
    }
}

/// A task handle that allows to get task result when the task is completed or cancel it if necessary.   
pub struct TaskHandle<T> {
    handle: task::JoinHandle<T>,
    cancel_event_sender: watch::Sender<bool>,
    canceled: bool,
}

/// Task execution error type. It is returned when [`TaskHandle`] is awaited in case the task is not completed.
/// Exit error status can be fetched from it.
#[derive(Debug)]
pub enum TaskError {
    /// Task has been canceled using [`TaskHandle::cancel`] method.
    Canceled,
    /// Task has been aborted using [`TaskHandle::abort`] method.
    Aborted,
    /// Task panicked. It contains an object with which the task panicked.
    Panicked(Box<dyn Any + Send>),
}

impl<T> TaskHandle<T> {
    /// Aborts the task associated with the handle.
    pub fn abort(&self) {
        self.handle.abort();
    }

    /// Cancels the task associated with the handle. Whether the task handles cancellation
    /// event (using [`crate::try_await`] macro) or not depends on task itself. If the task
    /// doesn't implement cancellation handling it continue execution until it finishes or being aborted.
    pub fn cancel(&mut self) {
        let _ = self.cancel_event_sender.send(true);
        self.canceled = true;
    }
}

impl<T> Future for TaskHandle<T> {
    type Output = Result<T, TaskError>;

    /// Polls for the task completion. The task could be completed successfully returning [`Ok`]
    /// or exited with an [`Err`]<[`TaskError`]> error. In the last case the
    /// actual reason (cancellation, abortion or panicking) could be fetched from it.
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.handle).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => match result {
                Ok(result) => {
                    if self.canceled {
                        Poll::Ready(Err(TaskError::Canceled))
                    } else {
                        Poll::Ready(Ok(result))
                    }
                }
                Err(handle_error) => {
                    if let Ok(reason) = handle_error.try_into_panic() {
                        Poll::Ready(Err(TaskError::Panicked(reason)))
                    } else {
                        Poll::Ready(Err(TaskError::Aborted))
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{is_task_canceled, spawn, TaskError};
    use crate::{try_await, AwaitResult::Canceled, AwaitResult::Completed};

    #[tokio::test]
    async fn test_task_cancellation() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(2);

        let cancelable_func = async move {
            let step1_result = try_await!(tokio::time::sleep(std::time::Duration::from_secs(0)));
            tx.try_send(step1_result).unwrap();

            let step2_result = try_await!(tokio::time::sleep(std::time::Duration::from_secs(u64::MAX)));
            tx.try_send(step2_result).unwrap();
        };

        let mut task_handle = spawn(cancelable_func);

        let step1_result = rx.recv().await.unwrap();
        assert_eq!(step1_result, Completed(()));

        task_handle.cancel();
        let step2_result = rx.recv().await.unwrap();
        assert_eq!(step2_result, Canceled);
    }

    #[tokio::test]
    async fn test_is_task_canceled() {
        let (tx, rx) = tokio::sync::oneshot::channel();

        let cancelable_func = async move {
            try_await!(tokio::time::sleep(std::time::Duration::from_secs(u64::MAX)));
            tx.send(is_task_canceled()).unwrap();
        };

        let mut task_handle = spawn(cancelable_func);
        task_handle.cancel();

        let is_task_canceled_result = rx.await.unwrap();
        assert_eq!(is_task_canceled_result, true);
    }

    #[tokio::test]
    async fn test_cancellation_order() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);

        let tx4 = tx.clone();
        let func4 = async move {
            try_await!(tokio::time::sleep(std::time::Duration::from_secs(u64::MAX)));
            tx4.try_send(4).unwrap();
        };

        let tx3 = tx.clone();
        let func3 = async move {
            try_await!(func4);
            tx3.try_send(3).unwrap();
        };

        let tx2 = tx.clone();
        let func2 = async move {
            try_await!(func3);
            tx2.try_send(2).unwrap();
        };

        let tx1 = tx.clone();
        let func1 = async move {
            try_await!(func2);
            tx1.try_send(1).unwrap();
        };

        let mut task_handle = spawn(func1);
        task_handle.cancel();

        let order = vec![
            rx.recv().await.unwrap(),
            rx.recv().await.unwrap(),
            rx.recv().await.unwrap(),
            rx.recv().await.unwrap(),
        ];
        assert_eq!(order, vec![4, 3, 2, 1]);
    }

    #[tokio::test]
    async fn test_task_abortion() {
        let cancelable_func = async move {
            try_await!(tokio::time::sleep(std::time::Duration::from_secs(u64::MAX)));
        };

        let task_handle = spawn(cancelable_func);
        task_handle.abort();

        let result = task_handle.await;
        match result {
            Err(TaskError::Aborted) => {}
            _ => assert!(false, "unexpected task result"),
        }
    }

    #[tokio::test]
    async fn test_task_panic() {
        let cancelable_func = async move {
            panic!("test message");
        };

        let task_handle = spawn(cancelable_func);
        let result = task_handle.await;
        match result {
            Err(TaskError::Panicked(panic_arg)) => {
                assert_eq!("test message", *panic_arg.downcast::<&str>().unwrap());
            }
            _ => assert!(false, "unexpected task result"),
        }
    }
}
