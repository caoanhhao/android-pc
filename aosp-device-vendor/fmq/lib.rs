//! Re-export of AOSP's `libfmq_rust` (crate `fmq`) for the android-pc HALs.

pub use fmq::{MQDescriptor, MessageQueue, ReadCompletion, SynchronizedReadWrite, WriteCompletion};
