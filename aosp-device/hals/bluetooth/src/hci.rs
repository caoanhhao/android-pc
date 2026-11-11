//! `IBluetoothHci`: one client session at a time over a [`Transport`].

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use android_hardware_bluetooth::aidl::android::hardware::bluetooth::{
    IBluetoothHci::IBluetoothHci, IBluetoothHciCallbacks::IBluetoothHciCallbacks, Status::Status,
};
use binder::{DeathRecipient, ExceptionCode, IBinder, Interface, Strong};
use log::{error, info, warn};

use crate::packet::{Packet, PacketType, HCI_RESET};
use crate::transport::{Receiver, Sender, Transport};

/// How long `initialize` waits for the controller to appear. Below the VTS init timeout,
/// so a machine without Bluetooth (QEMU) fails initialization instead of hanging it.
const OPEN_TIMEOUT: Duration = Duration::from_secs(3);
/// How long the controller has to answer HCI_Reset.
const RESET_TIMEOUT: Duration = Duration::from_secs(2);
/// How often the reader thread checks whether the session was closed.
const STOP_POLL: Duration = Duration::from_millis(100);
/// HCI Hardware Error event (code 0x10, one parameter: hardware code 0), sent to the
/// stack when the link to the controller breaks so it restarts Bluetooth.
const HARDWARE_ERROR: [u8; 3] = [0x10, 0x01, 0x00];

/// The Bluetooth HCI HAL.
pub struct BluetoothHci {
    transport: Transport,
    session: Arc<Mutex<Option<Session>>>,
}

/// An initialized client: the link to the controller and the thread forwarding its
/// packets to the client.
struct Session {
    sender: Sender,
    stop: Arc<AtomicBool>,
    reader: JoinHandle<()>,
    /// Closes the session when the client dies; unlinked on drop.
    _death: DeathRecipient,
}

impl Session {
    /// Stops the reader thread and closes the link, which gives a user-channel
    /// controller back to the kernel.
    fn close(self) {
        self.stop.store(true, Ordering::Relaxed);
        if self.reader.join().is_err() {
            error!("reader thread panicked");
        }
    }
}

impl BluetoothHci {
    pub fn new(transport: Transport) -> Self {
        Self { transport, session: Arc::new(Mutex::new(None)) }
    }

    /// Opens and resets the controller, then starts forwarding packets to `callbacks`.
    fn open(&self, callbacks: &Strong<dyn IBluetoothHciCallbacks>) -> Result<Session, Status> {
        let (sender, mut receiver) = self.transport.open(OPEN_TIMEOUT).map_err(|err| {
            warn!("cannot open {:?}: {err}", self.transport);
            Status::UNABLE_TO_OPEN_INTERFACE
        })?;
        reset(&sender, &mut receiver).map_err(|err| {
            error!("controller reset failed: {err}");
            Status::HARDWARE_INITIALIZATION_ERROR
        })?;

        let mut death = {
            let session = Arc::downgrade(&self.session);
            DeathRecipient::new(move || {
                warn!("client died, closing");
                close(&session);
            })
        };
        callbacks.as_binder().link_to_death(&mut death).map_err(|status| {
            error!("cannot watch the client: {status:?}");
            Status::UNKNOWN
        })?;

        let stop = Arc::new(AtomicBool::new(false));
        let reader = {
            let (callbacks, stop) = (callbacks.clone(), stop.clone());
            thread::Builder::new()
                .name("hci-reader".into())
                .spawn(move || forward(receiver, &callbacks, &stop))
                .map_err(|err| {
                    error!("cannot start the reader thread: {err}");
                    Status::UNKNOWN
                })?
        };
        Ok(Session { sender, stop, reader, _death: death })
    }

    fn send(&self, kind: PacketType, data: &[u8]) -> binder::Result<()> {
        let session = self.session.lock().expect("lock poisoned");
        let Some(session) = session.as_ref() else {
            return Err(ExceptionCode::ILLEGAL_STATE.into());
        };
        // A broken link also stops the reader thread, which reports a hardware error to
        // the stack; failing the binder call as well would only crash the client.
        if let Err(err) = session.sender.send(kind, data) {
            error!("cannot send {kind:?} packet: {err}");
        }
        Ok(())
    }
}

/// Closes the session behind `session`, if it is still open and the HAL still exists.
fn close(session: &Weak<Mutex<Option<Session>>>) {
    let Some(session) = session.upgrade() else {
        return;
    };
    // Take it out first: joining the reader must not hold the lock senders wait on.
    let taken = session.lock().expect("lock poisoned").take();
    if let Some(open) = taken {
        open.close();
    }
}

/// Sends HCI_Reset and waits for its Command Complete, dropping anything the controller
/// sent before (left over from an earlier session).
fn reset(sender: &Sender, receiver: &mut Receiver) -> io::Result<()> {
    sender.send(PacketType::Command, &HCI_RESET)?;
    let deadline = Instant::now() + RESET_TIMEOUT;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        if receiver.recv(left)?.is_some_and(|packet| packet.is_reset_complete()) {
            return Ok(());
        }
    }
    Err(io::Error::new(io::ErrorKind::TimedOut, "no HCI_Reset Command Complete"))
}

/// The reader thread: hands every controller packet to the client until `stop` is set.
fn forward(
    mut receiver: Receiver,
    callbacks: &Strong<dyn IBluetoothHciCallbacks>,
    stop: &AtomicBool,
) {
    while !stop.load(Ordering::Relaxed) {
        let Packet { kind, data } = match receiver.recv(STOP_POLL) {
            Ok(Some(packet)) => packet,
            Ok(None) => continue,
            Err(err) => {
                error!("controller link lost: {err}");
                if let Err(status) = callbacks.hciEventReceived(&HARDWARE_ERROR) {
                    warn!("cannot report the hardware error: {status:?}");
                }
                return;
            }
        };
        let delivered = match kind {
            PacketType::Event => callbacks.hciEventReceived(&data),
            PacketType::Acl => callbacks.aclDataReceived(&data),
            PacketType::Sco => callbacks.scoDataReceived(&data),
            PacketType::Iso => callbacks.isoDataReceived(&data),
            PacketType::Command => {
                warn!("dropping a command packet from the controller");
                continue;
            }
        };
        if let Err(status) = delivered {
            warn!("cannot deliver a {kind:?} packet: {status:?}");
        }
    }
}

impl Interface for BluetoothHci {}

impl IBluetoothHci for BluetoothHci {
    fn initialize(&self, callbacks: &Strong<dyn IBluetoothHciCallbacks>) -> binder::Result<()> {
        info!("initialize");
        // Held while opening, so a second client gets ALREADY_INITIALIZED, not a race.
        let mut session = self.session.lock().expect("lock poisoned");
        let status = if session.is_some() {
            Status::ALREADY_INITIALIZED
        } else {
            match self.open(callbacks) {
                Ok(open) => {
                    *session = Some(open);
                    Status::SUCCESS
                }
                Err(status) => status,
            }
        };
        // The client may send its first command from inside the callback.
        drop(session);
        info!("initialization complete: {status:?}");
        callbacks.initializationComplete(status)
    }

    fn close(&self) -> binder::Result<()> {
        info!("close");
        close(&Arc::downgrade(&self.session));
        Ok(())
    }

    fn sendHciCommand(&self, command: &[u8]) -> binder::Result<()> {
        self.send(PacketType::Command, command)
    }

    fn sendAclData(&self, data: &[u8]) -> binder::Result<()> {
        self.send(PacketType::Acl, data)
    }

    fn sendScoData(&self, data: &[u8]) -> binder::Result<()> {
        self.send(PacketType::Sco, data)
    }

    fn sendIsoData(&self, data: &[u8]) -> binder::Result<()> {
        self.send(PacketType::Iso, data)
    }
}
