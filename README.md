# autopool
![Rust](https://github.com/ragona/autopool/workflows/Rust/badge.svg)

> :warning: **Project in early development!** This is not a stable library.

This is an attempt to use a [PID controller](https://en.wikipedia.org/wiki/PID_controller) to drive a worker pool.
Most worker pools require you to set a number of workers.
However, the right number of workers can change based on environment conditions.
`autopool` adds or removes workers to achieve a goal throughput.

