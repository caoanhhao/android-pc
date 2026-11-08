//! Legacy 8259 PIC pair: remapped away from the exception vectors and fully
//! masked. All device interrupts go through the (IO)APIC.

use x86_64::instructions::port::Port;

/// Remapped vector base; the 16 vectors here only ever see spurious IRQs.
pub const VECTOR_BASE: u8 = 0xe0;

pub fn disable() {
    unsafe {
        let mut cmd1 = Port::<u8>::new(0x20);
        let mut data1 = Port::<u8>::new(0x21);
        let mut cmd2 = Port::<u8>::new(0xa0);
        let mut data2 = Port::<u8>::new(0xa1);
        let mut wait = Port::<u8>::new(0x80);
        let mut io = |port: &mut Port<u8>, v: u8| {
            port.write(v);
            wait.write(0);
        };
        io(&mut cmd1, 0x11); // ICW1: init, expect ICW4
        io(&mut cmd2, 0x11);
        io(&mut data1, VECTOR_BASE); // ICW2: vector offsets
        io(&mut data2, VECTOR_BASE + 8);
        io(&mut data1, 4); // ICW3: slave on IRQ2
        io(&mut data2, 2);
        io(&mut data1, 0x01); // ICW4: 8086 mode
        io(&mut data2, 0x01);
        io(&mut data1, 0xff); // mask everything
        io(&mut data2, 0xff);
    }
}
