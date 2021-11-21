//! Task manager implementation.

use log;
use tokio::macros::support::Future;
use tokio::sync::mpsc;

use crate::task::{spawn_inner, TaskError, TaskHandle};

/// Task manager builder. Provides methods for task manager initialization and configuration.
#[derive(Copy, Clone)]
pub struct TaskManagerBuilder {
    max_tasks: usize,
    capacity: usize,
    completion_events_buffer_size: usize,
}

impl TaskManagerBuilder {
    /// Sets max task count the manager will be handling
    pub fn with_max_tasks(&mut self, max_tasks: usize) -> &mut TaskManagerBuilder {
        self.max_tasks = max_tasks;
        return self;
    }

    /// Sets slab task storage initial capacity. The right capacity choice prevents extra memory allocation.
    pub fn with_capacity(&mut self, capacity: usize) -> &mut TaskManagerBuilder {
        self.capacity = capacity;
        return self;
    }

    /// Sets completion event queue size. Too small queue size could prevent tasks from immediate
    /// cancellation until manager handles events from another tasks and empties the queue.
    pub fn with_completion_event_buffer_size(
        &mut self,
        completion_event_buffer_size: usize,
    ) -> &mut TaskManagerBuilder {
        self.completion_events_buffer_size = completion_event_buffer_size;
        return self;
    }

    /// Builds [`TaskManager`] instance using provided configurations.
    pub fn build(self) -> TaskManager {
        TaskManager::new(self.max_tasks, self.capacity, self.completion_events_buffer_size)
    }
}

/// Task manager error.
#[derive(Debug)]
pub enum TaskManagerError {
    /// Requested task not found.
    TaskNotFound,
    /// Task count limit is exceeded.
    TaskManagerIsFull,
}

/// Task manager is an asynchronous task supervisor that stores all spawned tasks, controls its states
/// and provides an api for task management.
pub struct TaskManager {
    tasks: slab::Slab<TaskHandle<()>>,
    completion_event_queue_sender: mpsc::Sender<usize>,
    completion_event_queue_receiver: mpsc::Receiver<usize>,
    max_tasks: usize,
}

impl TaskManager {
    /// Returns a task manager builder.
    pub fn builder() -> TaskManagerBuilder {
        TaskManagerBuilder {
            max_tasks: 1024,
            capacity: 32,
            completion_events_buffer_size: 256,
        }
    }

    /// Creates a new task manager instance.
    pub fn new(max_tasks: usize, capacity: usize, completion_events_buffer_size: usize) -> TaskManager {
        let (completion_event_queue_sender, completion_event_queue_receiver) =
            mpsc::channel(completion_events_buffer_size);

        TaskManager {
            tasks: slab::Slab::with_capacity(capacity),
            completion_event_queue_sender,
            completion_event_queue_receiver,
            max_tasks,
        }
    }

    /// Returns manager task count.
    pub fn size(&self) -> usize {
        self.tasks.len()
    }

    /// Spawns a new asynchronous task wrapping it to be supervised by the task manager.
    /// Method can return [`None`] if task manager is full and task can not be spawned yet
    /// otherwise it returns task key that can be used to cancel this task.
    pub fn try_spawn<F>(&mut self, future: F) -> Option<usize>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        if self.tasks.len() == self.max_tasks {
            return None;
        }

        let task_entry = self.tasks.vacant_entry();
        let task_key = task_entry.key();

        let completion_event_queue_sender = self.completion_event_queue_sender.clone();
        let task_handle = spawn_inner(future, async move {
            let _ = completion_event_queue_sender.send(task_key).await;
        });
        task_entry.insert(task_handle);

        return Some(task_key);
    }

    /// Runs manager processing loop handling task events.
    /// Method is cancellation safe and can be used in `tokio::select!` macro.
    pub async fn process(&mut self) {
        loop {
            let task_key = self
                .completion_event_queue_receiver
                .recv()
                .await
                .expect("channel unexpectedly closed");

            match self.tasks.try_remove(task_key) {
                None => log::debug!("task {} is not longer attached to the manager", task_key),
                Some(task_handle) => {
                    let _ = task_handle.await;
                }
            }
        }
    }

    /// Detaches a task from the manager. The task is not longer supervised by the manager.
    pub fn detach(&mut self, task_key: usize) -> Result<TaskHandle<()>, TaskManagerError> {
        match self.tasks.try_remove(task_key) {
            Some(task_handle) => Ok(task_handle),
            None => Err(TaskManagerError::TaskNotFound),
        }
    }

    /// Cancels all supervised tasks.
    pub fn cancel(mut self) {
        for (_, task_handle) in self.tasks.iter_mut() {
            task_handle.cancel();
        }
    }

    /// Aborts all supervised tasks, consuming self.
    pub fn abort(mut self) {
        for (_, task_handle) in self.tasks.iter_mut() {
            task_handle.abort();
        }
    }

    /// Waits until all the tasks are completed consuming self.
    /// If `resume_panic` argument is `true ` and any of the tasks panic
    /// method resumes the panic on the current task. It is useful in test environment
    /// when you want your application to be panicked if any of the spawned tasks panic.
    pub async fn join(mut self, resume_panic: bool) {
        for (_, task_handle) in std::mem::take(&mut self.tasks) {
            match task_handle.await {
                Err(TaskError::Panicked(reason)) => {
                    if resume_panic {
                        std::panic::resume_unwind(reason);
                    }
                }
                _ => {}
            }
        }
    }

    /// Cancels a particular task by task key returned by [`TaskManager::try_spawn`] method.
    /// If task not found (task key is wrong or task already finished)
    /// method returns [`TaskManagerError::TaskNotFound`] error.
    pub fn cancel_task(&mut self, task_key: usize) -> Result<(), TaskManagerError> {
        match self.tasks.get_mut(task_key) {
            Some(task_handle) => {
                task_handle.cancel();
                Ok(())
            }
            None => Err(TaskManagerError::TaskNotFound),
        }
    }

    /// Aborts a task by a task key.
    /// The task is removed from the storage so that it can't be accessed anymore.
    pub fn abort_task(&mut self, task_key: usize) -> Result<(), TaskManagerError> {
        match self.tasks.try_remove(task_key) {
            Some(task_handle) => {
                task_handle.abort();
                Ok(())
            }
            None => Err(TaskManagerError::TaskNotFound),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::TaskManager;
    use crate::try_await;

    #[tokio::test]
    async fn test_task_manager_overflow() {
        let mut task_manager = TaskManager::builder().with_max_tasks(1).build();

        let task_key = task_manager.try_spawn(async {});
        assert!(task_key.is_some());

        let task_key = task_manager.try_spawn(async {});
        assert!(task_key.is_none());
    }

    #[tokio::test]
    #[should_panic(expected = "test panic")]
    async fn test_task_unwinding_enabled() {
        let panic_func = async { panic!("test panic") };

        let mut task_manager = TaskManager::builder().build();
        task_manager.try_spawn(panic_func).unwrap();
        task_manager.join(true).await;
    }

    #[tokio::test]
    async fn test_task_unwinding_disabled() {
        let panic_func = async { panic!("test panic") };

        let mut task_manager = TaskManager::builder().build();
        task_manager.try_spawn(panic_func).unwrap();
        task_manager.join(false).await;
    }

    #[tokio::test]
    async fn test_task_abortion() {
        let infinite_func = async {
            tokio::time::sleep(std::time::Duration::from_secs(u64::MAX)).await;
        };

        let mut task_manager = TaskManager::builder().build();
        let task_key = task_manager.try_spawn(infinite_func).unwrap();
        task_manager.abort_task(task_key).unwrap();
        task_manager.join(true).await;
    }

    #[tokio::test]
    async fn test_task_manager_abortion() {
        let infinite_func1 = async {
            tokio::time::sleep(std::time::Duration::from_secs(u64::MAX)).await;
        };
        let infinite_func2 = async {
            tokio::time::sleep(std::time::Duration::from_secs(u64::MAX)).await;
        };

        let mut task_manager = TaskManager::builder().build();
        task_manager.try_spawn(infinite_func1).unwrap();
        task_manager.try_spawn(infinite_func2).unwrap();

        task_manager.abort();
    }

    #[tokio::test]
    async fn test_task_manager_cancellation() {
        let cancelable_func1 = async move {
            try_await!(tokio::time::sleep(std::time::Duration::from_secs(u64::MAX)));
        };
        let cancelable_func2 = async move {
            try_await!(tokio::time::sleep(std::time::Duration::from_secs(u64::MAX)));
        };

        let mut task_manager = TaskManager::builder().build();
        task_manager.try_spawn(cancelable_func1).unwrap();
        task_manager.try_spawn(cancelable_func2).unwrap();

        task_manager.cancel();
    }

    #[tokio::test]
    async fn test_processing_loop() {
        let mut task_manager = TaskManager::builder().build();
        task_manager.try_spawn(async {}).unwrap();
        task_manager.try_spawn(async {}).unwrap();
        assert_eq!(task_manager.size(), 2);

        tokio::task::yield_now().await;
        tokio::time::timeout(Duration::from_millis(0), task_manager.process())
            .await
            .unwrap_err();
        assert_eq!(task_manager.size(), 0);

        task_manager.try_spawn(async {}).unwrap();
        assert_eq!(task_manager.size(), 1);

        tokio::task::yield_now().await;
        tokio::time::timeout(Duration::from_millis(0), task_manager.process())
            .await
            .unwrap_err();
        assert_eq!(task_manager.size(), 0);
    }

    #[tokio::test]
    async fn test_task_detach() {
        let cancelable_func = async move {
            try_await!(tokio::time::sleep(std::time::Duration::from_secs(u64::MAX)));
        };

        let mut task_manager = TaskManager::builder().build();
        let task_key = task_manager.try_spawn(cancelable_func).unwrap();

        let mut task_handle = task_manager.detach(task_key).unwrap();
        assert_eq!(task_manager.size(), 0);

        task_handle.cancel();
        let _ = task_handle.await;
    }
}
