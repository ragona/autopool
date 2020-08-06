#![allow(dead_code)]

use crate::{CrossbeamReceiver, CrossbeamSender, WorkerPoolStatus};
use async_std::{
    prelude::*,
    sync::{channel, Receiver, Sender, TryRecvError, TrySendError},
};
use std::{collections::VecDeque, fmt::Debug};

const FIX_ME: usize = 128;

/// # WorkerPool
///
/// This is a channels-oriented async worker pool.
/// It's intended to be used with relatively long-running futures that all write out to the
/// same output channel of type `Out`. The worker pool gathers all of that output in whatever
/// order it appears, and sends it to the output channel.
///
/// The number of workers in this implementation is intended as a best effort, not a fixed
/// count, with an eye towards being used in situations where we may want that number to go
/// up or down over time based on the environment conditions.
///
/// You could imagine that a system under load might decide to back off on the number of open
/// connections if it was experiencing resource contention, and conversely to add new workers
/// if the queue has grown and we aren't at our max worker count.
///
/// I'm not incredibly concerned about allocations in this model; `WorkerPool` is a higher level
/// abstraction than something like `crossbeam`.
///
pub struct WorkerPool<In, Out, F> {
    /// The async function that a worker performs
    task: fn(Job<In, Out>) -> F,
    /// User-configurable settings
    config: PoolConfig<In>,
    /// How many workers we actually have
    cur_workers: usize,
    /// Outstanding tasks
    queue: VecDeque<In>,
    /// Channel for completed work from workers
    workers_channel: (Sender<Out>, Receiver<Out>),
    /// Channel to stop workers early
    close_channel: (Sender<()>, Receiver<()>),
    /// Channel for WorkerEvents from the workers
    worker_events: (CrossbeamSender<WorkerEvent>, CrossbeamReceiver<WorkerEvent>),
    /// Channel to accept WorkerPoolCommands from external sources
    command_events: (CrossbeamSender<WorkerPoolCommand>, CrossbeamReceiver<WorkerPoolCommand>),
    /// How many workers we have asked to stop that are still working
    outstanding_stops: usize,
}

#[derive(Debug, Copy, Clone)]
pub struct PoolConfig<In> {
    /// How many workers we want
    target_workers: usize,
    /// Default job that will be assigned to idle workers when the queue is empty
    default_job: Option<In>,
    /// Worker cap
    max_workers: usize,
}

#[derive(Debug, Copy, Clone)]
enum WorkerEvent {
    WorkerDone,
    WorkerStopped,
}

#[derive(Debug, Copy, Clone)]
pub enum WorkerPoolCommand {
    Stop,
    SetWorkerCount(usize),
}

pub struct Job<In, Out> {
    pub task: In,
    pub close: Receiver<()>,
    pub results: Sender<Out>,
}

#[derive(Copy, Clone)]
pub enum JobStatus {
    Done,
    Stopped,
    Working,
}

impl<In, Out, F> WorkerPool<In, Out, F>
where
    In: Send + Sync + Clone + 'static,
    Out: Send + Sync + 'static,
    F: Future<Output = JobStatus> + Send + 'static,
{
    pub fn new(task: fn(Job<In, Out>) -> F) -> Self {
        Self::new_with_config(task, PoolConfig::default())
    }

    pub fn new_with_config(task: fn(Job<In, Out>) -> F, config: PoolConfig<In>) -> Self {
        Self {
            workers_channel: channel(config.max_workers),
            close_channel: channel(config.max_workers),
            command_events: crossbeam_channel::unbounded(),
            worker_events: crossbeam_channel::unbounded(),
            queue: Default::default(),
            outstanding_stops: 0,
            cur_workers: 0,
            config,
            task,
        }
    }

    /// Number of workers currently working
    /// This is the number of workers we haven't tried to stop yet plus the workers that haven't
    /// noticed they were told to stop.
    pub fn cur_workers(&self) -> usize {
        self.cur_workers - self.outstanding_stops
    }

    /// Target number of workers
    pub fn target_workers(&self) -> usize {
        self.config.target_workers
    }

    /// Whether the current number of workers is the target number of workers
    /// Adjusted for the number of workers that we have TOLD to stop but have
    /// not actually gotten around to stopping yet.
    pub fn at_target_worker_count(&self) -> bool {
        self.cur_workers() == self.target_workers()
    }

    pub fn working(&self) -> bool {
        self.cur_workers() > 0
    }

    /// Sets the target number of workers.
    /// Does not stop in-progress workers.
    pub fn set_target_workers(&mut self, n: usize) {
        self.config.target_workers = n;
    }

    /// Add a new task to the back of the queue
    pub fn push(&mut self, task: In) {
        self.queue.push_back(task);
    }

    pub fn command(&mut self, command: WorkerPoolCommand) {
        self.command_events.0.send(command).expect("failed to send command");
    }

    pub fn work(&mut self) -> WorkerPoolStatus<Out> {
        self.process_pool_commands();
        self.process_worker_events();
        self.balance_workers();

        match self.workers_channel.1.try_recv() {
            Ok(out) => WorkerPoolStatus::Ready(out),
            Err(e) => match e {
                TryRecvError::Empty => WorkerPoolStatus::Working,
                TryRecvError::Disconnected => WorkerPoolStatus::Done,
            },
        };

        WorkerPoolStatus::Working
    }

    fn process_pool_commands(&mut self) {
        while let Ok(command) = self.command_events.1.try_recv() {
            match command {
                WorkerPoolCommand::Stop => {
                    for _ in 0..self.config.target_workers {
                        self.send_stop_work_message();
                    }
                }
                WorkerPoolCommand::SetWorkerCount(n) => {
                    let n = match n {
                        0 => 1,
                        n => n,
                    };

                    self.config.target_workers = n;
                }
            }
        }
    }

    fn process_worker_events(&mut self) {
        while let Ok(event) = self.worker_events.1.try_recv() {
            match event {
                WorkerEvent::WorkerDone => {
                    self.cur_workers -= 1;
                }
                WorkerEvent::WorkerStopped => {
                    self.cur_workers -= 1;
                    self.outstanding_stops -= 1;
                }
            }
        }
    }

    /// Starts a new worker if there is work to do.
    /// There is work to do either if there is an outstanding queue,
    /// or if this pool has a default task that can be assigned.
    fn start_worker(&mut self) {
        let task = self.get_task();
        if task.is_none() {
            return;
        }

        let work_send = self.workers_channel.0.clone();
        let close_recv = self.close_channel.1.clone();
        let event_send = self.worker_events.0.clone();
        let job = Job::new(task.unwrap(), close_recv, work_send);
        let fut = (self.task)(job);

        // If a worker stops on its own without us telling it to stop then we want to know about
        // it so that we can spin up a replacement. This is done through an unbounded crossbeam
        // channel that is processed every tick to update state.
        async_std::task::spawn(async move {
            let status = fut.await;
            let message = match status {
                JobStatus::Done => WorkerEvent::WorkerDone,
                JobStatus::Stopped => WorkerEvent::WorkerStopped,
                JobStatus::Working => panic!("worker stopped while running, unexpected state"),
            };

            event_send.send(message).expect("failed to send WorkerEvent");
        });

        self.cur_workers += 1;
    }

    fn get_task(&mut self) -> Option<In> {
        if self.queue.is_empty() {
            return match &self.config.default_job {
                None => None,
                Some(default) => Some(default.clone()),
            };
        } else {
            Some(self.queue.pop_front().unwrap())
        }
    }

    /// Adds a single worker if we are under our target count
    /// Sends a single cancel message if we are over our target count
    fn balance_workers(&mut self) {
        if self.cur_workers() < self.target_workers() {
            self.start_worker();
        } else if self.cur_workers() > self.target_workers() {
            self.send_stop_work_message();
        }
    }

    /// Find a listening worker and tell it to stop.
    /// Doesn't forcibly kill in-progress tasks.
    fn send_stop_work_message(&mut self) {
        loop {
            match self.close_channel.0.try_send(()) {
                Ok(_) => break,
                Err(e) => match e {
                    TrySendError::Full(_) => {}
                    TrySendError::Disconnected(_) => panic!("foo"),
                },
            }
        }
    }
}

impl<In, Out> Job<In, Out> {
    pub fn new(task: In, close: Receiver<()>, results: Sender<Out>) -> Self {
        Self { task, close, results }
    }

    pub fn stop_requested(&self) -> bool {
        match self.close.try_recv() {
            Ok(_) => true,
            Err(_) => false,
        }
    }
}

impl<In> Default for PoolConfig<In> {
    fn default() -> Self {
        Self { target_workers: 8, default_job: None, max_workers: 1024 }
    }
}

impl<In> PoolConfig<In> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn default_job(&mut self, job: In) -> &mut Self {
        self.default_job = Some(job);
        self
    }

    pub fn target_workers(&mut self, n: usize) -> &mut Self {
        self.target_workers = n;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    async fn pool_new() {
        let mut _pool = WorkerPool::new(double);
    }

    #[async_test]
    async fn pool_new_with_config() {
        let _pool = WorkerPool::new_with_config(
            double,
            *PoolConfig::new().target_workers(4).default_job((2, 10)),
        );
    }
}
