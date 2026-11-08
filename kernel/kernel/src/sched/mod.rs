//! Scheduler (not implemented yet).
//!
//! Planned (phase B1): kernel threads and Linux tasks (`clone` threads and
//! processes) with per-CPU run queues, preemption from the LAPIC timer,
//! futex wait queues, and `sched_*` syscalls. Load balancing comes later.
