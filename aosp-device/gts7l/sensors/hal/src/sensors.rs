//! ISensors: one non-wake-up accelerometer (the DSP's LSM6DSO), events into the framework's
//! event FMQ.

use std::sync::{Arc, Mutex};

use android_hardware_sensors::aidl::android::hardware::sensors::{
    Event::{
        Event,
        EventPayload::{
            EventPayload,
            MetaData::{MetaData, MetaDataEventType::MetaDataEventType},
            Vec3::Vec3,
        },
    },
    ISensors::{
        self, ISensors as ISensorsTrait, OperationMode::OperationMode, RateLevel::RateLevel,
        SharedMemInfo::SharedMemInfo,
    },
    ISensorsCallback::ISensorsCallback,
    SensorInfo::{self, SensorInfo as SensorInfoParcel},
    SensorStatus::SensorStatus,
    SensorType::SensorType,
};
use binder::{ExceptionCode, Interface, Status, Strong};
use log::{info, warn};
use pcfmq::{MQDescriptor, MessageQueue, SynchronizedReadWrite};

use crate::stream::{Sample, Stream};

const ACCEL_HANDLE: i32 = 1;
/// The rates the DSP offers for the LSM6DSO accelerometer go from 26 to 832 Hz; Android
/// asks at most for 200 Hz (SENSOR_DELAY_FASTEST is 0, clamped to minDelay).
const MIN_RATE_HZ: u32 = 26;
const MAX_RATE_HZ: u32 = 208;
/// Range of the stock configuration (lsm6dso_0_8g.json): +/-8 g, 16 bits.
const MAX_RANGE: f32 = 8.0 * 9.80665;
const RESOLUTION: f32 = MAX_RANGE / 32768.0;

/// The SSC reports the accelerometer in Android's device frame already (its registry
/// carries the board's axis mapping, which Samsung's own HAL passes through): x to the right
/// and y up along the portrait screen, z out of it. Checked on the tablet (docs/gts7l.md §19).
fn to_device_frame(v: [f32; 3]) -> [f32; 3] {
    v
}

/// The framework's event FMQ.
struct EventQueue(MessageQueue<Event>);

impl EventQueue {
    /// Writes one event and wakes the framework's reader; drops it when the queue is full.
    fn post(&mut self, event: Event) {
        let Some(mut write) = self.0.write_many(1) else {
            warn!("event queue full, sample dropped");
            return;
        };
        if write.write(event).is_err() {
            return;
        }
        drop(write);
        let Some(word) = self.0.event_flag_word() else { return };
        let bits = ISensors::EVENT_QUEUE_FLAG_BITS_READ_AND_PROCESS as u32;
        // EventFlag::wake: set the bits, FUTEX_WAKE_BITSET for those not set before.
        let old = word.fetch_or(bits, std::sync::atomic::Ordering::SeqCst);
        if !old & bits != 0 {
            // SAFETY: the word lives in the FMQ's shared memory as long as the queue does,
            // which outlives this call; FUTEX_WAKE_BITSET only reads its address.
            unsafe {
                libc::syscall(
                    libc::SYS_futex,
                    word.as_ptr(),
                    libc::FUTEX_WAKE_BITSET,
                    i32::MAX,
                    std::ptr::null::<libc::timespec>(),
                    std::ptr::null::<u32>(),
                    bits,
                )
            };
        }
    }
}

struct Accel {
    active: bool,
    rate_hz: u32,
    stream: Option<Stream>,
}

pub struct Sensors {
    queue: Arc<Mutex<Option<EventQueue>>>,
    // For dynamic sensors, which there are none of; kept as the interface asks.
    _callback: Mutex<Option<Strong<dyn ISensorsCallback>>>,
    accel: Mutex<Accel>,
}

impl Interface for Sensors {}

impl Sensors {
    pub fn new() -> Self {
        Self {
            queue: Arc::new(Mutex::new(None)),
            _callback: Mutex::new(None),
            accel: Mutex::new(Accel { active: false, rate_hz: MIN_RATE_HZ, stream: None }),
        }
    }

    /// Starts, restarts (new rate) or stops the stream to match `accel`.
    fn apply(&self, accel: &mut Accel) {
        let running = accel.stream.is_some();
        if !accel.active {
            if running {
                info!("accelerometer off");
            }
            accel.stream = None;
            return;
        }
        // Stop the old stream (and its sensord) before the next one enables the sensor.
        accel.stream = None;
        let queue = Arc::clone(&self.queue);
        match Stream::start("accel", accel.rate_hz, move |sample| {
            if let Some(queue) = queue.lock().unwrap().as_mut() {
                queue.post(accel_event(sample, now_ns()));
            }
        }) {
            Ok(stream) => accel.stream = Some(stream),
            Err(err) => warn!("cannot start the accelerometer stream: {err}"),
        }
    }
}

fn accel_info() -> SensorInfoParcel {
    SensorInfoParcel {
        sensorHandle: ACCEL_HANDLE,
        name: "LSM6DSO Accelerometer".into(),
        vendor: "STMicroelectronics".into(),
        version: 1,
        r#type: SensorType::ACCELEROMETER,
        typeAsString: String::new(),
        maxRange: MAX_RANGE,
        resolution: RESOLUTION,
        power: 0.17,
        minDelayUs: (1_000_000 / MAX_RATE_HZ) as i32,
        maxDelayUs: (1_000_000 / MIN_RATE_HZ) as i32,
        fifoReservedEventCount: 0,
        fifoMaxEventCount: 0,
        requiredPermission: String::new(),
        flags: SensorInfo::SENSOR_FLAG_BITS_CONTINUOUS_MODE,
    }
}

/// The DSP rate for a sampling period: the nearest one it offers at or above the request.
fn rate_for_period(period_ns: i64) -> u32 {
    let hz = if period_ns <= 0 { MAX_RATE_HZ as i64 } else { (1_000_000_000 + period_ns - 1) / period_ns };
    (hz.clamp(MIN_RATE_HZ as i64, MAX_RATE_HZ as i64)) as u32
}

fn accel_event(sample: Sample, timestamp: i64) -> Event {
    let [x, y, z] = to_device_frame(sample.values);
    let status = match sample.accuracy {
        a if a <= 0 => SensorStatus::UNRELIABLE,
        1 => SensorStatus::ACCURACY_LOW,
        2 => SensorStatus::ACCURACY_MEDIUM,
        _ => SensorStatus::ACCURACY_HIGH,
    };
    Event {
        timestamp,
        sensorHandle: ACCEL_HANDLE,
        sensorType: SensorType::ACCELEROMETER,
        payload: EventPayload::Vec3(Vec3 { x, y, z, status }),
    }
}

/// Event timestamps are in the elapsedRealtimeNanos() clock (CLOCK_BOOTTIME).
fn now_ns() -> i64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: ts is a valid, writable timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) };
    ts.tv_sec * 1_000_000_000 + ts.tv_nsec
}

fn check_handle(handle: i32) -> binder::Result<()> {
    if handle == ACCEL_HANDLE {
        Ok(())
    } else {
        Err(Status::new_exception(ExceptionCode::ILLEGAL_ARGUMENT, None))
    }
}

fn unsupported<T>() -> binder::Result<T> {
    Err(Status::new_exception(ExceptionCode::UNSUPPORTED_OPERATION, None))
}

impl ISensorsTrait for Sensors {
    fn activate(&self, handle: i32, enabled: bool) -> binder::Result<()> {
        check_handle(handle)?;
        let mut accel = self.accel.lock().unwrap();
        if accel.active != enabled {
            accel.active = enabled;
            self.apply(&mut accel);
        }
        Ok(())
    }

    fn batch(&self, handle: i32, period_ns: i64, _max_latency_ns: i64) -> binder::Result<()> {
        check_handle(handle)?;
        let rate_hz = rate_for_period(period_ns);
        let mut accel = self.accel.lock().unwrap();
        if accel.rate_hz != rate_hz {
            accel.rate_hz = rate_hz;
            if accel.active {
                self.apply(&mut accel);
            }
        }
        Ok(())
    }

    fn configDirectReport(&self, _: i32, _: i32, _: RateLevel) -> binder::Result<i32> {
        unsupported()
    }

    fn flush(&self, handle: i32) -> binder::Result<()> {
        check_handle(handle)?;
        if !self.accel.lock().unwrap().active {
            return Err(Status::new_exception(ExceptionCode::ILLEGAL_ARGUMENT, None));
        }
        // No FIFO: every sample is already in the queue, so the flush completes at once.
        if let Some(queue) = self.queue.lock().unwrap().as_mut() {
            queue.post(Event {
                timestamp: 0,
                sensorHandle: handle,
                sensorType: SensorType::META_DATA,
                payload: EventPayload::Meta(MetaData {
                    what: MetaDataEventType::META_DATA_FLUSH_COMPLETE,
                }),
            });
        }
        Ok(())
    }

    fn getSensorsList(&self) -> binder::Result<Vec<SensorInfoParcel>> {
        Ok(vec![accel_info()])
    }

    fn initialize(
        &self,
        event_queue: &MQDescriptor<Event, SynchronizedReadWrite>,
        _wake_lock_queue: &MQDescriptor<i32, SynchronizedReadWrite>,
        callback: &Strong<dyn ISensorsCallback>,
    ) -> binder::Result<()> {
        // A new framework (sensorservice restarted): every sensor starts disabled. The
        // wake lock queue stays unused, no sensor here wakes the system.
        {
            let mut accel = self.accel.lock().unwrap();
            accel.active = false;
            self.apply(&mut accel);
        }
        *self.queue.lock().unwrap() = Some(EventQueue(MessageQueue::from_desc(event_queue, true)));
        *self._callback.lock().unwrap() = Some(callback.clone());
        info!("initialized");
        Ok(())
    }

    fn injectSensorData(&self, _: &Event) -> binder::Result<()> {
        unsupported()
    }

    fn registerDirectChannel(&self, _: &SharedMemInfo) -> binder::Result<i32> {
        unsupported()
    }

    fn setOperationMode(&self, mode: OperationMode) -> binder::Result<()> {
        if mode == OperationMode::NORMAL {
            Ok(())
        } else {
            unsupported()
        }
    }

    fn unregisterDirectChannel(&self, _: i32) -> binder::Result<()> {
        unsupported()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_follow_the_period() {
        assert_eq!(rate_for_period(66_666_666), MIN_RATE_HZ); // SENSOR_DELAY_UI, 15 Hz
        assert_eq!(rate_for_period(20_000_000), 50); // SENSOR_DELAY_GAME
        assert_eq!(rate_for_period(5_000_000), 200);
        assert_eq!(rate_for_period(1_000_000), MAX_RATE_HZ);
        assert_eq!(rate_for_period(0), MAX_RATE_HZ);
    }

    #[test]
    fn samples_become_accel_events() {
        let event = accel_event(Sample { values: [0.0, 9.8, 0.5], accuracy: 3 }, 42);
        assert_eq!(event.timestamp, 42);
        assert_eq!(event.sensorType, SensorType::ACCELEROMETER);
        match event.payload {
            EventPayload::Vec3(v) => {
                assert_eq!((v.x, v.y, v.z), (0.0, 9.8, 0.5));
                assert_eq!(v.status, SensorStatus::ACCURACY_HIGH);
            }
            other => panic!("not a vec3: {other:?}"),
        }
    }

    #[test]
    fn the_list_has_the_accelerometer() {
        let info = accel_info();
        assert_eq!(info.sensorHandle, ACCEL_HANDLE);
        assert_eq!(info.minDelayUs, 4807);
        assert_eq!(info.maxDelayUs, 38461);
    }
}
