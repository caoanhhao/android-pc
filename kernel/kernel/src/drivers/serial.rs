//! 16550 UART on COM1, used as the kernel console and log sink.

use core::fmt::{self, Write};
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;
use x86_64::instructions::port::Port;

const COM1: u16 = 0x3f8;

pub struct SerialPort {
    base: u16,
}

impl SerialPort {
    const fn new(base: u16) -> Self {
        Self { base }
    }

    fn outb(&self, offset: u16, value: u8) {
        unsafe { Port::new(self.base + offset).write(value) }
    }

    fn inb(&self, offset: u16) -> u8 {
        unsafe { Port::new(self.base + offset).read() }
    }

    fn init(&mut self) {
        self.outb(1, 0x00); // disable UART interrupts
        self.outb(3, 0x80); // DLAB on
        self.outb(0, 0x01); // divisor 1 => 115200 baud
        self.outb(1, 0x00);
        self.outb(3, 0x03); // 8N1, DLAB off
        self.outb(2, 0xc7); // enable + clear FIFOs, 14-byte threshold
        self.outb(4, 0x0b); // DTR | RTS | OUT2
    }

    fn write_byte(&mut self, byte: u8) {
        // Wait for the transmit holding register to be empty.
        while self.inb(5) & 0x20 == 0 {
            core::hint::spin_loop();
        }
        self.outb(0, byte);
    }
}

impl Write for SerialPort {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(b);
        }
        Ok(())
    }
}

static COM1_PORT: Mutex<SerialPort> = Mutex::new(SerialPort::new(COM1));

pub fn init() {
    COM1_PORT.lock().init();
}

/// Release the console lock unconditionally. Only for the panic path.
pub unsafe fn force_unlock() {
    unsafe { COM1_PORT.force_unlock() }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    // Interrupts off so an interrupt handler that prints cannot deadlock on
    // the lock held by the code it interrupted.
    without_interrupts(|| {
        let _ = COM1_PORT.lock().write_fmt(args);
    });
}

/// Write bytes as they are (the tty did the output processing).
pub fn write_raw(data: &[u8]) {
    without_interrupts(|| {
        let mut port = COM1_PORT.lock();
        for &b in data {
            port.write_byte(b);
        }
    });
}

/// A received byte, if any. Only the BSP's timer tick reads (no lock: the
/// line status and receive registers are not touched by the writers).
pub fn read_byte() -> Option<u8> {
    let port = SerialPort::new(COM1);
    (port.inb(5) & 1 != 0).then(|| port.inb(0))
}

/// Print to the serial console.
#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => ($crate::drivers::serial::_print(format_args!($($arg)*)));
}

/// Print a line to the serial console.
#[macro_export]
macro_rules! kprintln {
    () => ($crate::kprint!("\n"));
    ($($arg:tt)*) => ($crate::kprint!("{}\n", format_args!($($arg)*)));
}
