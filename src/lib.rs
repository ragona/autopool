//! # autopool
//!
//! > Warning! **Project in early development!** This is not a stable library.
//!
//! This is an attempt to use a [PID controller](https://en.wikipedia.org/wiki/PID_controller) to drive a worker pool.
//! Most worker pools require you to set a number of workers.
//! However, the right number of workers can change based on environment conditions.
//! `autopool` adds or removes workers to achieve a target throughput.
//!
//!

#![allow(dead_code)]

mod autopool;
mod pid;
mod pool;
mod tracker;

#[cfg(feature = "tuning")]
pub mod tuning;

pub use crate::{
    autopool::{AutoPool, AutoPoolConfig},
    pid::PidController,
    pool::{Job, JobStatus, WorkerPool, WorkerPoolCommand, WorkerPoolConfig},
};

pub(crate) use crossbeam_channel::{
    self, Receiver as CrossbeamReceiver, Sender as CrossbeamSender,
};

pub type JobFunction<In, Out, F> = fn(Job<In, Out>) -> F;

pub enum WorkerPoolStatus<Out> {
    Ready(Out),
    Working,
    Done,
}

pub(crate) use tracker::TickWorkTracker;

#[cfg(test)]
mod tests {
    #[test]
    fn stub_test() {}
}
