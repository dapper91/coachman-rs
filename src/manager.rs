//! Task manager utils

use log;
use tokio::macros::support::Future;
use tokio::sync::{mpsc, watch};
use tokio::task;

/// Task manager builder. Provides methods for task manager initialization and configuration.
#[derive(Copy, Clone)]
pub struct TaskBuilder {
    max_tasks: usize,
    capacity: usize,
    completion_events_buffer_size: usize,
}

impl TaskBuilder {
    /// Sets max task count the manager will be handling
    pub fn with_max_tasks(&mut self, max_tasks: usize) -> &mut TaskBuilder {
        self.max_tasks = max_tasks;
        return self;
    }

    /// Sets slab task storage initial capacity. The right capacity choice prevents extra memory allocation.
    pub fn with_capacity(&mut self, capacity: usize) -> &mut TaskBuilder {
        self.capacity = capacity;
        return self;
    }

    /// Sets completion events queue size. Too small queue size could prevent tasks from immediate
    /// cancellation until manager handles events from another tasks and empties a the queue.
    pub fn with_completion_event_buffer_size(&mut self, completion_event_buffer_size: usize) -> &mut TaskBuilder {
        self.completion_events_buffer_size = completion_event_buffer_size;
        return self;
    }

    /// Builds [`TaskManager`] instance using provided configurations.
    pub fn build(self) -> TaskManager {
        TaskManager::new(self.max_tasks, self.capacity, self.completion_events_buffer_size)
    }
}

tokio::task_local! {
    pub static TASK_CANCEL_EVENT: watch::Receiver<bool>;
}

/// Returns `true` if current task is cancelled.
pub fn is_task_canceled() -> bool {
    *TASK_CANCEL_EVENT.with(|event| event.clone()).borrow()
}

struct TaskHandle {
    handle: task::JoinHandle<()>,
    cancel_event_sender: watch::Sender<bool>,
}

/// Task manager is an asynchronous task supervisor that stores all spawned tasks, controls its states
/// and provides an api from task management.
pub struct TaskManager {
    tasks: slab::Slab<TaskHandle>,
    completion_event_queue_sender: mpsc::Sender<usize>,
    completion_event_queue_receiver: mpsc::Receiver<usize>,
    max_tasks: usize,
}

impl TaskManager {
    /// Returns a task manager builder.
    pub fn builder() -> TaskBuilder {
        TaskBuilder {
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

    /// Spawns a new asynchronous task wrapping it to be supervised by the task manager.
    /// Method can return [`None`] if task manager is full and task can not be spawned yet
    /// otherwise it returns task key that can be used to cancel this task.
    pub fn try_spawn<F>(&mut self, future: F) -> Option<usize>
    where
        F: Future<Output = ()> + Send + 'static,
        F::Output: Send + 'static,
    {
        if self.tasks.len() == self.max_tasks {
            return None;
        }

        let (cancel_event_sender, cancel_event_receiver) = watch::channel(false);
        let task_entry = self.tasks.vacant_entry();
        let task_key = task_entry.key();

        let completion_event_queue_sender = self.completion_event_queue_sender.clone();
        let handle = tokio::spawn(TASK_CANCEL_EVENT.scope(cancel_event_receiver, async move {
            future.await;
            completion_event_queue_sender.send(task_key).await;
        }));

        task_entry.insert(TaskHandle {
            handle,
            cancel_event_sender,
        });

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

            if self.tasks.try_remove(task_key).is_none() {
                log::debug!("task {} not found", task_key);
            }
        }
    }

    /// Cancels all supervised tasks returning theirs handles.
    pub fn cancel(self) -> Vec<task::JoinHandle<()>> {
        let mut handles = Vec::new();

        for (_, task_handle) in self.tasks {
            task_handle.cancel_event_sender.send(true);
            handles.push(task_handle.handle);
        }

        return handles;
    }

    /// Cancels a particular task by task key returned by [`TaskManager::try_spawn`] method.
    /// If the task has been completed or canceled already it returns [`None`].
    pub fn cancel_task(&mut self, task_key: usize) -> Option<task::JoinHandle<()>> {
        if let Some(handle) = self.tasks.try_remove(task_key) {
            handle.cancel_event_sender.send(true);
            Some(handle.handle)
        } else {
            None
        }
    }

    /// Aborts all supervised tasks.
    pub async fn abort(self) {
        for (_, task_handle) in self.tasks {
            task_handle.handle.abort();
            task_handle.handle.await;
        }
    }
}
