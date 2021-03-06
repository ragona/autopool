use crate::{
    JobStatus, PidController, TickWorkTracker, WorkerPool, WorkerPoolCommand, WorkerPoolStatus,
};

use async_std::future::Future;

use log::debug;

use async_std::{
    pin::Pin,
    stream::Stream,
    task::{Context, Poll},
};
use std::time::Duration;

pub struct AutoPool<In, Out, F> {
    /// WorkerPool to drive and monitor
    pool: WorkerPool<In, Out, F>,
    /// Current number
    num_workers: f32,
    /// Three part controller that drives towards the target rate
    pid: PidController,
    /// Work tracking
    tracker: TickWorkTracker,
    /// User controlled settings
    config: AutoPoolConfig,
}

pub struct AutoPoolConfig {
    /// How often to evaluate the PID controller and add/remove workers
    tick_rate: Duration,
    /// Target executions per second across all workers
    goal_rate_per_sec: f32,
}

impl<In, Out, F> AutoPool<In, Out, F>
where
    In: Send + Sync + Clone + 'static,
    Out: Send + Sync + 'static,
    F: Future<Output = JobStatus> + Send + 'static,
{
    pub fn new(config: AutoPoolConfig, pool: WorkerPool<In, Out, F>) -> Self {
        Self {
            tracker: TickWorkTracker::new(config.tick_rate),
            pid: Default::default(), // todo PidConfig
            num_workers: 0.0,
            config,
            pool,
        }
    }

    fn work(&mut self) -> WorkerPoolStatus<Out> {
        self.tick();

        match self.pool.work() {
            WorkerPoolStatus::Ready(out) => {
                self.tracker.track_work();
                return WorkerPoolStatus::Ready(out);
            }
            WorkerPoolStatus::Working => WorkerPoolStatus::Working,
            WorkerPoolStatus::Done => WorkerPoolStatus::Done,
        }
    }

    fn tick(&mut self) {
        if !self.tracker.tick_done() {
            return;
        }

        debug!("{}, {}, {}", self.pid.output(), self.tracker.tick_rate_per_sec(), self.num_workers);

        self.pid.update(self.config.goal_rate_per_sec, self.tracker.tick_rate_per_sec());
        self.num_workers += self.pid.output();

        // Update workers if we've crossed over an integer threshold
        if self.num_workers.floor() as usize != self.pool.target_workers() {
            self.pool.command(WorkerPoolCommand::SetWorkerCount(self.num_workers as usize));
        }
    }
}

impl<In, Out, F> Stream for AutoPool<In, Out, F>
where
    In: Send + Sync + Unpin + Clone + 'static,
    Out: Send + Sync + Unpin + 'static,
    F: Future<Output = JobStatus> + Send + 'static,
{
    type Item = Out;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        dbg!("YOOO");
        match self.get_mut().work() {
            WorkerPoolStatus::Ready(o) => Poll::Ready(Some(o)),
            WorkerPoolStatus::Working => Poll::Pending,
            WorkerPoolStatus::Done => Poll::Ready(None),
        }
    }
}

impl AutoPoolConfig {
    pub fn new(tick_rate: Duration, goal_rate_per_sec: f32) -> Self {
        Self { tick_rate, goal_rate_per_sec }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{pool::WorkerPoolConfig, Job};
    use async_std::task;
    use futures_await_test::async_test;
    use std::time::Duration;

    /// Double the input some number of times or until we receive a close message
    async fn double(job: Job<(usize, usize), usize>) -> JobStatus {
        let (mut i, n) = job.task;
        for _ in 0..n {
            // play nice with the pool by allowing it to stop this loop early
            if job.stop_requested() {
                break;
            }

            // do the actual work
            i *= 2;

            // send it to the pool for collection so it can be sent along to listeners
            job.results.send(i).await;

            // pretend this is hard
            task::sleep(Duration::from_millis(100)).await;
        }

        JobStatus::Done
    }

    #[async_test]
    async fn autopool_new() {
        let wp_config = *WorkerPoolConfig::new().target_workers(4).default_job((2, 10));
        let ap_config = AutoPoolConfig::new(Duration::from_secs_f32(0.1), 2500.0);
        let worker_pool = WorkerPool::new_with_config(double, wp_config);

        let _auto_pool = AutoPool::new(ap_config, worker_pool);
    }
}
