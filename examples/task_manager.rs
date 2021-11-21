use coachman as cm;
use coachman::try_await;
use coachman::{Canceled, Completed};
use std::time;

async fn inner(i: usize) {
    match try_await!(tokio::time::sleep(time::Duration::from_secs(i as u64))) {
        Canceled => println!("task#{} inner canceled", i),
        Completed(_) => println!("task#{} inner completed", i),
    };
}

async fn outer(i: usize) {
    match try_await!(inner(i)) {
        Canceled => println!("task#{} outer canceled", i),
        Completed(_) => println!("task#{} outer completed", i),
    }

    println!("task#{} canceled: {}", i, cm::is_task_canceled());
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut task_manager = cm::TaskManager::builder().with_max_tasks(10).with_capacity(10).build();

    let mut task_keys = Vec::new();
    for i in 0..10 {
        let task_key = task_manager.try_spawn(outer(i)).unwrap();
        task_keys.push(task_key)
    }

    tokio::time::timeout(time::Duration::from_secs(5), task_manager.process(false)).await;

    for task_key in task_keys {
        if task_manager.cancel_task(task_key).is_ok() {
            println!("task-{} canceled", task_key)
        } else {
            println!("task-{} already finished", task_key)
        }
    }

    task_manager.join(true).await;
}
