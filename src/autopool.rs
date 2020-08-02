use crate::pool::{PoolEvent, WorkerPoolStatus};
use crate::CrossbeamReceiver;
use crate::CrossbeamSender;
use crate::{Job, JobStatus, PidController, WorkerPool, WorkerPoolCommand};
use async_std::future::Future;
use async_std::pin::Pin;
use async_std::stream::Stream;
use async_std::sync::channel;
use async_std::task;
use async_std::task::{Context, Poll};
use crossbeam_channel::{RecvError, TryRecvError};
use log::debug;
use std::borrow::BorrowMut;
use std::{
    fmt::{Debug, Formatter},
    time::{Duration, Instant},
};
use typed_builder;

#[derive(TypedBuilder)]
pub struct AutoPool<In, Out, F> {
    /// WorkerPool to drive and monitor
    pool: WorkerPool<In, Out, F>,
    /// Float representation of how many workers to start at.
    #[builder(default = 1.0)]
    num_workers: f32,
    /// Target executions per second across all workers
    #[builder(default)]
    rate_per_sec: f32,
    /// Three part controller that drives towards the target rate
    #[builder(default)]
    pid: PidController,
    /// todo: Make this output PoolEvent::TaskComplete(Out)
    #[builder(default)]
    output: EventChannel<PoolEvent>,
}

pub struct EventChannel<T> {
    chan: (CrossbeamSender<T>, CrossbeamReceiver<T>),
}

impl<T> EventChannel<T> {
    pub fn new() -> Self {
        Self { chan: crossbeam_channel::unbounded() }
    }

    pub fn try_next(&mut self) -> Option<T> {
        match self.chan.1.try_recv() {
            Ok(o) => Some(o),
            Err(e) => None,
        }
    }

    pub fn next(&mut self) -> Result<T, RecvError> {
        self.chan.1.recv()
    }
}

impl<T> Default for EventChannel<T> {
    fn default() -> Self {
        Self::new()
    }
}
impl<In, Out, F> AutoPool<In, Out, F>
where
    In: Send + Sync + 'static,
    Out: Send + Sync + 'static,
    F: Future<Output = JobStatus> + Send + 'static,
{
    /// Collects results from the workers, updates controllers, sends completed results
    /// This method is meant to resolve very quickly, but it's limited by the async state
    /// machine in `WorkerPool`.
    ///
    /// todo: Enumerate scenarios that cause actual blocking
    ///
    pub fn work(&mut self) -> Option<Out> {
        //
        // try to flush outstanding events from the pool
        //
        loop {
            match self.pool.mut_pool_events().try_recv() {
                Ok(event) => {
                    match event {
                        PoolEvent::WorkReady(out) => return Some(out),
                        PoolEvent::TickComplete(tick) => {
                            debug!(
                                "{}, {}, {}",
                                self.pid.output(),
                                tick.tracker.rate_per_sec(),
                                self.num_workers,
                            );

                            self.pid.update(self.rate_per_sec, tick.tracker.rate_per_sec());
                            self.num_workers += self.pid.output();

                            // Update workers if we've crossed over an integer threshold
                            if self.num_workers.floor() as usize != self.pool.target_workers() {
                                commands.send(WorkerPoolCommand::SetWorkerCount(
                                    self.num_workers as usize,
                                ));
                            }
                        }
                    }
                }
                _ => {
                    break;
                }
            }
        }
        //
        // This part is also synchronous, but it is invoking the asyncronous part of the application
        // Its goal is to generate PoolEvents for the next invocation of this loop.
        // This will minimally block.
        //
        match self.pool.work() {
            WorkerPoolStatus::Working => {}
            WorkerPoolStatus::Done => {}
        }

        None
    }
}

impl<In, Out, F> Stream for AutoPool<In, Out, F>
where
    In: Send + Sync + Unpin + 'static,
    Out: Send + Sync + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    type Item = Out;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut ap = self.get_mut();
        let mut out = &ap.output;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    async fn pool_test() {
        // let ap = AutoPool::
    }
}
