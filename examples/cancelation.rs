use coachman as cm;
use coachman::{try_await, Canceled, Completed, TaskError};

async fn inner_func(i: usize, duration: u64) {
    match try_await!(tokio::time::sleep(std::time::Duration::from_secs(duration))) {
        Canceled => println!("task#{} inner canceled", i),
        Completed(_) => println!("task#{} inner completed", i),
    }
}

async fn outer_func(i: usize, duration: u64) {
    match try_await!(inner_func(i, duration)) {
        Canceled => println!("task#{} outer canceled", i),
        Completed(_) => println!("task#{} outer completed", i),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut task_handles = Vec::new();
    for i in 0..5 {
        let duration = i as u64;
        task_handles.push(cm::spawn(outer_func(i, duration)));
    }

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    for (i, mut handle) in task_handles.into_iter().enumerate() {
        if tokio::time::timeout_at(deadline, &mut handle).await.is_ok() {
            println!("task-{} completed", i);
        } else {
            handle.cancel();
            match handle.await {
                Result::Err(TaskError::Canceled) => println!("task-{} canceled", i),
                Result::Err(TaskError::Aborted) => println!("task-{} aborted", i),
                Result::Err(TaskError::Panicked(_)) => println!("task-{} panicked", i),
                Result::Ok(_) => unreachable!(),
            }
        }
    }
}
