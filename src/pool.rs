#![allow(dead_code)]

use crate::CrossbeamReceiver;
use crate::CrossbeamSender;
use async_std::{
    prelude::*,
    sync::{channel, Receiver, Sender},
    task,
};
use std::collections::VecDeque;
use std::fmt::{Debug, Formatter};
use std::time::{Duration, Instant};

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
    /// How often to evaluate and adjust the worker pool
    tick_rate: Duration,
    /// How many workers we want
    num_workers: usize,
    /// How many workers we actually have
    cur_workers: usize,
    /// Worker cap
    max_workers: usize,
    /// Information about total work completed by this pool
    tracker: WorkTracker,
    /// Outstanding tasks
    queue: VecDeque<In>,
    /// Channel for completed work from workers
    workers_channel: (Sender<Out>, Receiver<Out>),
    /// Channel to send completed work to external sources
    output_channel: (Sender<Out>, Receiver<Out>),
    /// Channel to stop workers early
    close_channel: (Sender<()>, Receiver<()>),
    /// Channel for WorkerEvents from the workers
    worker_events: (CrossbeamSender<WorkerEvent>, CrossbeamReceiver<WorkerEvent>),
    /// Channel to accept WorkerPoolCommands from external sources
    command_events: (CrossbeamSender<WorkerPoolCommand>, CrossbeamReceiver<WorkerPoolCommand>),
    /// Channel to send PoolEvents to inform external sources
    pool_events: (CrossbeamSender<PoolEvent<Out>>, CrossbeamReceiver<PoolEvent<Out>>),
    /// How many workers we have asked to stop that are still working
    outstanding_stops: usize,
    /// Information work performed in the current tick
    pub tick: Tick,
}

#[derive(Debug, Copy, Clone)]
pub enum PoolEvent<Out> {
    TickComplete(Tick),
    WorkReady(Out),
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

// todo command channel

pub struct Job<In, Out> {
    pub task: In,
    pub close: Receiver<()>,
    pub results: Sender<Out>,
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

pub enum JobStatus {
    Done,
    Stopped,
    Working,
}

impl<In, Out, F> WorkerPool<In, Out, F>
where
    In: Send + Sync + 'static,
    Out: Send + Sync + 'static,
    F: Future<Output = JobStatus> + Send + 'static,
{
    pub fn new(task: fn(Job<In, Out>) -> F) -> Self {
        Self {
            task,
            tick_rate: Duration::from_secs_f32(0.1),
            num_workers: 1,
            cur_workers: 0,
            max_workers: 1024, // todo make all of this configurable with sensible defaults
            workers_channel: channel(16),
            close_channel: channel(16),
            worker_events: crossbeam_channel::unbounded(),
            command_events: crossbeam_channel::unbounded(),
            queue: VecDeque::with_capacity(16),
            pool_events: crossbeam_channel::unbounded(),
            outstanding_stops: 0,
            tracker: WorkTracker::new(),
            tick: Tick::new(Duration::from_secs_f32(0.1)),
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
        self.num_workers
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
        self.num_workers = n;
    }

    /// Add a new task to the back of the queue
    pub fn push(&mut self, task: In) {
        self.queue.push_back(task);
    }

    pub fn command_events(&self) -> crossbeam_channel::Sender<WorkerPoolCommand> {
        self.command_events.0.clone()
    }

    pub fn mut_pool_events(&mut self) -> &mut crossbeam_channel::Receiver<PoolEvent> {
        &mut self.pool_events.1
    }

    pub fn work(&mut self) -> WorkerPoolStatus {
        task::block_on(async {
            self.flush_output().await;

            if !self.event_loop() {
                return WorkerPoolStatus::Done;
            }

            self.balance_workers().await;

            if !self.working() {
                return WorkerPoolStatus::Done;
            }

            WorkerPoolStatus::Working
        })
    }

    /// Processes outstanding command and worker events
    /// Returns whether or not to continue execution.
    fn event_loop(&mut self) -> bool {
        //
        // update listeners
        //
        if self.tick.done() {
            self.pool_events
                .0
                .send(PoolEvent::TickComplete(self.tick))
                .expect("failed to send event into unbounded channel, this shouldn't happen!");
            self.tick = Tick::new(self.tick_rate);
        }

        //
        // worker events
        //
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

        //
        // command events
        //
        while let Ok(command) = self.command_events.1.try_recv() {
            match command {
                WorkerPoolCommand::Stop => {
                    return false;
                }
                WorkerPoolCommand::SetWorkerCount(n) => {
                    let n = match n {
                        0 => 1,
                        n => n,
                    };

                    println!("{}, {}", n, self.num_workers); // todo log
                    self.num_workers = n;
                }
            }
        }

        true
    }

    /// Flush all outstanding work results to the output channel.
    ///
    /// This blocks on consumption, which gives us a nice property -- if a user only
    /// wants a limited number of messages they can just read a limited number of times.
    /// This ends up only updating the async state machine that number of times, which
    /// is the "lazy" property of async we wanted to achieve.
    async fn flush_output(&mut self) {
        while let Ok(out) = self.workers_channel.1.try_recv() {
            self.tracker.track();
            self.tick.tracker.track();

            // uh give this out to someone! todo
        }
    }

    /// Starts a new worker if there is work to do
    fn start_worker(&mut self) {
        if self.queue.is_empty() {
            return;
        }

        let task = self.queue.pop_front().unwrap();
        let work_send = self.workers_channel.0.clone();
        let close_recv = self.close_channel.1.clone();
        let event_send = self.worker_events.0.clone();
        let job = Job::new(task, close_recv, work_send);
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

    /// Find a listening worker and tell it to stop.
    /// Doesn't forcibly kill in-progress tasks.
    async fn send_stop_work_message(&mut self) {
        self.outstanding_stops += 1;
        self.close_channel.0.send(()).await;
    }

    /// Adds a single worker if we are under our target count
    /// Sends a single cancel message if we are over our target count
    async fn balance_workers(&mut self) {
        if self.cur_workers() < self.target_workers() {
            self.start_worker();
        } else if self.cur_workers() > self.target_workers() {
            self.send_stop_work_message().await;
        }
    }
}

#[derive(Copy, Clone)]
pub struct WorkTracker {
    count: usize,
    start: Instant,
}

impl WorkTracker {
    pub fn new() -> Self {
        Self { start: Instant::now(), count: 0 }
    }

    pub fn count(&self) -> usize {
        self.count
    }

    pub fn track(&mut self) {
        self.count += 1;
    }

    pub fn rate_per_sec(&self) -> f32 {
        self.count() as f32 / Instant::now().duration_since(self.start).as_secs_f32()
    }
}

impl Debug for WorkTracker {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestTracker")
            .field("rate", &self.rate_per_sec())
            .field("count", &self.count())
            .finish()
    }
}

#[derive(Debug, Copy, Clone)]
pub struct Tick {
    pub start: Instant,
    pub end: Instant,
    pub tracker: WorkTracker,
}

impl Tick {
    pub fn new(duration: Duration) -> Self {
        let now = Instant::now();
        Self { start: now, end: now + duration, tracker: WorkTracker::new() }
    }

    pub fn done(&self) -> bool {
        Instant::now() >= self.end
    }

    pub fn ms_late(&self) -> f32 {
        if !self.done() {
            return 0.0;
        }

        Instant::now().duration_since(self.start).as_secs_f32()
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
    async fn pool_test() {
        let mut pool = WorkerPool::new(double);
    }
}
