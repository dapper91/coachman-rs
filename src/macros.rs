//! `coachman` package macros.

pub use AwaitResult::{Cancelled, Completed};

/// `AwaitResult` represent `try_await!` macro result providing two values: [`Cancelled`], [`Completed`].
#[derive(Eq, PartialEq)]
pub enum AwaitResult<T> {
    /// Current task is cancelled.
    Cancelled,
    /// Future is completed with value `T`.
    Completed(T),
}

impl<T> AwaitResult<T> {
    /// Returns `true` if current task has been canceled.
    pub fn is_canceled(&self) -> bool {
        match self {
            Cancelled => true,
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
            AwaitResult::Cancelled => None,
            AwaitResult::Completed(res) => Some(res),
        }
    }
}

/// Similar to `await` keywords but support cancellation. If current task has been canceled `try_await!`
/// returns [`Cancelled`] otherwise it waits for the future readiness and returns [`Completed`]
/// where `T` is the result of the future.
///
/// **NOTE:** The future must be cancellation safe.
///
/// Basic usage:
/// ```
/// match try_await!(tokio::time::sleep(std::time::Duration::from_secs(5))) {
///     Cancelled => println!("task cancelled"),
///     Completed(_) => println!("sleep completed"),
/// };
/// ```
#[macro_export]
macro_rules! try_await {
    ($fut: expr) => {{
        use coachman::AwaitResult;
        use tokio::select;

        let mut cancel_event = $crate::manager::TASK_CANCEL_EVENT.with(|event| event.clone());

        select! {
            Ok(result) = cancel_event.changed() => { AwaitResult::Cancelled },
            result = $fut => { AwaitResult::Completed(result) }
        }
    }};
}
