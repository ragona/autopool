#![allow(dead_code)]

mod autopool;
mod pid;
mod pool;
mod tracker;

#[cfg(feature = "tuning")]
pub mod tuning;

pub use pid::PidController;
pub use pool::{Job, JobStatus, WorkerPool, WorkerPoolCommand};

pub(crate) use crossbeam_channel::{
    self, Receiver as CrossbeamReceiver, Sender as CrossbeamSender,
};

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
