//! `coachman` package macros.

pub use AwaitResult::{Canceled, Completed};

/// `AwaitResult` represent [`try_await!`] macro result providing two values: [`Canceled`], [`Completed`].
#[derive(Debug, Eq, PartialEq)]
pub enum AwaitResult<T> {
    /// Current task is canceled.
    Canceled,
    /// Future is completed with value `T`.
    Completed(T),
}

impl<T> AwaitResult<T> {
    /// Returns `true` if current task has been canceled.
    pub fn is_canceled(&self) -> bool {
        match self {
            Canceled => true,
            _ => false,
        }
    }

    /// Returns `true` if the future is completed.
    pub fn is_completed(&self) -> bool {
        return !self.is_canceled();
    }

    /// Converts AwaitResult<T> to [`Option<T>`], consuming self.
    pub fn result(self) -> Option<T> {
        match self {
            AwaitResult::Canceled => None,
            AwaitResult::Completed(res) => Some(res),
        }
    }
}

/// Similar to `await` keywords but support cancellation. If current task has been canceled `try_await!`
/// returns [`Canceled`] otherwise it waits for the future readiness and returns [`Completed`]
/// where `T` is the result of the future.
///
/// **NOTE:** The future must be cancellation safe.
///
/// Basic usage:
/// ```
/// use coachman::{try_await, Canceled, Completed};
///
/// async fn cancelable_func() {
///     match try_await!(tokio::time::sleep(std::time::Duration::from_secs(5))) {
///         Canceled => println!("task canceled"),
///         Completed(_) => println!("sleep completed"),
///     };
/// }
/// ```
#[macro_export]
macro_rules! try_await {
    ($fut: expr) => {{
        use tokio::select;
        use $crate::AwaitResult;

        let mut cancel_event = $crate::task::TASK_CANCEL_EVENT.with(|event| event.clone());

        select! {
            biased; // we should handle task cancellation event in the deepest functions on the stack first
            result = $fut => {
                if *cancel_event.borrow() {
                    AwaitResult::Canceled
                } else {
                    AwaitResult::Completed(result)
                }
            }
            Ok(_) = cancel_event.changed() => {
                AwaitResult::Canceled
            },
        }
    }};
}
