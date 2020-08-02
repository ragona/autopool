#[macro_use]
extern crate typed_builder;

mod autopool;
mod pid;
mod pool;

#[cfg(feature = "tuning")]
pub mod tuning;

pub use pid::PidController;
pub use pool::{Job, JobStatus, WorkerPool, WorkerPoolCommand};

pub(crate) use crossbeam_channel::{
    self, Receiver as CrossbeamReceiver, Sender as CrossbeamSender,
};

pub enum WorkerPoolStatus {
    Working,
    Done,
}

#[cfg(test)]
mod tests {
    #[test]
    fn stub_test() {}
}
