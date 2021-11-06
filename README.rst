========
CoachMan
========

.. image:: img/logo.png
  :width: 256
  :alt: coachman


``coachman`` is a rust asynchronous task manager built on top of tokio framework.
The main feature of ``coachman`` is making asynchronous tasks cancellable.
Look at the following example:

.. code-block:: rust

    use coachman as cm;
    use coachman::try_await;
    use coachman::{Cancelled, Completed};
    use std::time;

    async fn function(i: usize) {
        match try_await!(tokio::time::sleep(time::Duration::from_secs(i as u64))) {
            Cancelled => println!("task#{} cancelled", i),
            Completed(_) => println!("task#{} completed", i),
        };

        println!("task#{} canceled: {}", i, cm::is_task_canceled());
    }

    #[tokio::main(flavor = "current_thread")]
    async fn main() {
        let mut task_manager = cm::TaskManager::builder().with_max_tasks(10).with_capacity(10).build();

        let mut task_keys = Vec::new();
        for i in 0..10 {
            let task_key = task_manager.try_spawn(function(i)).unwrap();
            task_keys.push(task_key)
        }

        tokio::time::timeout(time::Duration::from_secs(5), task_manager.process()).await;

        let mut task_handles = Vec::new();
        for task_key in task_keys {
            if let Some(handle) = task_manager.cancel_task(task_key) {
                task_handles.push(handle);
            } else {
                println!("task-{} already finished", task_key)
            }
        }

        for task_handle in task_handles {
            task_handle.await;
        }
    }
