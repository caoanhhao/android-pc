//! Samsung Galaxy Tab S7 LTE (gts7l) fingerprint sensor: a Goodix GW36T1 in the power key,
//! read only by Samsung's `securefp` TrustZone app. This crate drives that app through our
//! `/dev/qseecom` and the sensor's power and interrupt through `/dev/goodix_fp`
//! (docs/gts7l.md §25, docs/gts7l-fingerprint-tz.md).

pub mod engine;
pub mod qsee;
pub mod securefp;
pub mod sensor;
pub mod store;
