//! Samples of one DSP sensor, from a `gts7l-sensord --stream` child process.
//!
//! sensord prints one line per sample, `accel t=109.475979 9.2316 -0.1700 3.4403 accuracy=3`
//! (DSP time in seconds, the values, the SSC accuracy 0-3), and disables the sensor on
//! SIGTERM. A child that ends on its own (DSP not up yet, DSP restart) is started again.

use std::io::{self, BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use log::{info, warn};

const SENSORD: &str = "/vendor/bin/gts7l-sensord";
/// Seconds sensord waits for the DSP's sensor service to appear.
const SERVICE_WAIT_S: &str = "30";
const RESTART_DELAY: Duration = Duration::from_secs(2);
const STOP_POLL: Duration = Duration::from_millis(100);

/// One sample: the three values and the SSC accuracy (0 unreliable to 3 high).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub values: [f32; 3],
    pub accuracy: i8,
}

/// Parses a sample line of `kind`; None for comments (`# enabled`) and anything else.
pub fn parse_sample(kind: &str, line: &str) -> Option<Sample> {
    let mut words = line.split_ascii_whitespace();
    if words.next()? != kind || !words.next()?.starts_with("t=") {
        return None;
    }
    let mut values = [0.0; 3];
    for value in &mut values {
        *value = words.next()?.parse().ok()?;
    }
    let accuracy = words.next()?.strip_prefix("accuracy=")?.parse().ok()?;
    Some(Sample { values, accuracy })
}

/// A running stream; dropping it stops sensord, which disables the sensor.
pub struct Stream {
    stop: Arc<AtomicBool>,
    pid: Arc<Mutex<Option<i32>>>,
    thread: Option<JoinHandle<()>>,
}

impl Stream {
    /// Streams `kind` at `rate_hz`, calling `sink` for every sample from a thread of its own.
    pub fn start(
        kind: &'static str,
        rate_hz: u32,
        sink: impl Fn(Sample) + Send + 'static,
    ) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let pid = Arc::new(Mutex::new(None));
        let thread = {
            let (stop, pid) = (Arc::clone(&stop), Arc::clone(&pid));
            thread::Builder::new()
                .name(format!("stream-{kind}"))
                .spawn(move || run(kind, rate_hz, &sink, &stop, &pid))?
        };
        Ok(Self { stop, pid, thread: Some(thread) })
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(pid) = *self.pid.lock().unwrap() {
            terminate(pid);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn terminate(pid: i32) {
    // SAFETY: kill() has no memory effects; pid is our own child, not yet reaped (the
    // thread clears it before wait()).
    unsafe { libc::kill(pid, libc::SIGTERM) };
}

fn run(
    kind: &'static str,
    rate_hz: u32,
    sink: &dyn Fn(Sample),
    stop: &AtomicBool,
    pid: &Mutex<Option<i32>>,
) {
    while !stop.load(Ordering::SeqCst) {
        match spawn(kind, rate_hz) {
            Ok(mut child) => {
                *pid.lock().unwrap() = Some(child.id() as i32);
                // A stop between the check above and storing the pid found no pid to signal.
                if stop.load(Ordering::SeqCst) {
                    terminate(child.id() as i32);
                }
                info!("streaming {kind} at {rate_hz} Hz (sensord pid {})", child.id());
                pump(kind, &mut child, sink);
                *pid.lock().unwrap() = None;
                let status = child.wait();
                if !stop.load(Ordering::SeqCst) {
                    warn!("sensord {kind} stream ended ({status:?}), starting it again");
                }
            }
            Err(err) => warn!("cannot start {SENSORD}: {err}"),
        }
        let mut waited = Duration::ZERO;
        while waited < RESTART_DELAY && !stop.load(Ordering::SeqCst) {
            thread::sleep(STOP_POLL);
            waited += STOP_POLL;
        }
    }
}

fn spawn(kind: &str, rate_hz: u32) -> io::Result<Child> {
    Command::new(SENSORD)
        .args(["--stream", kind, "--rate", &rate_hz.to_string(), "--wait", SERVICE_WAIT_S])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

/// Reads samples until sensord exits; its error lines go to the log.
fn pump(kind: &str, child: &mut Child, sink: &dyn Fn(Sample)) {
    let stderr = child.stderr.take().map(|err| {
        thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                warn!("sensord: {line}");
            }
        })
    });
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if let Some(sample) = parse_sample(kind, &line) {
                sink(sample);
            }
        }
    }
    if let Some(stderr) = stderr {
        let _ = stderr.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_samples() {
        assert_eq!(
            parse_sample("accel", "accel t=109.475979 9.2316 -0.1700 3.4403 accuracy=3"),
            Some(Sample { values: [9.2316, -0.17, 3.4403], accuracy: 3 })
        );
    }

    #[test]
    fn skips_other_lines() {
        assert_eq!(parse_sample("accel", "# enabled"), None);
        assert_eq!(parse_sample("accel", "# event 1022, 54 bytes"), None);
        assert_eq!(parse_sample("accel", "gyro t=1.0 0.1 0.2 0.3 accuracy=3"), None);
        assert_eq!(parse_sample("accel", "accel t=1.0 0.1 0.2"), None);
        assert_eq!(parse_sample("accel", "accel t=1.0 0.1 x 0.3 accuracy=3"), None);
    }
}
