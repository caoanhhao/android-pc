//! Global Descriptor Table and Task State Segment (one of each per CPU).
//!
//! The layout is fixed so that `syscall`/`sysret` work in phase B1:
//! STAR[47:32] = KERNEL_CS (kernel CS, SS = +8) and STAR[63:48] = KERNEL_DS
//! (`sysret` loads SS = +8 = USER_DS and CS = +16 = USER_CS, RPL 3).

use x86_64::PrivilegeLevel::{Ring0, Ring3};
use x86_64::instructions::segmentation::{CS, DS, ES, SS, Segment};
use x86_64::instructions::tables::load_tss;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;

pub const KERNEL_CS: SegmentSelector = SegmentSelector::new(1, Ring0);
pub const KERNEL_DS: SegmentSelector = SegmentSelector::new(2, Ring0);
#[allow(dead_code)]
pub const USER_DS: SegmentSelector = SegmentSelector::new(3, Ring3);
#[allow(dead_code)]
pub const USER_CS: SegmentSelector = SegmentSelector::new(4, Ring3);
pub const TSS: SegmentSelector = SegmentSelector::new(5, Ring0);

/// Fill `gdt` with the standard layout, load it and the TSS on this CPU.
///
/// # Safety
/// Must be called once per CPU with structures that live forever.
pub unsafe fn load(gdt: &'static mut GlobalDescriptorTable, tss: &'static TaskStateSegment) {
    let selectors = [
        gdt.append(Descriptor::kernel_code_segment()),
        gdt.append(Descriptor::kernel_data_segment()),
        gdt.append(Descriptor::user_data_segment()),
        gdt.append(Descriptor::user_code_segment()),
        gdt.append(Descriptor::tss_segment(tss)),
    ];
    let expected = [KERNEL_CS, KERNEL_DS, USER_DS, USER_CS, TSS];
    for (got, want) in selectors.iter().zip(expected) {
        assert_eq!(got.0, want.0, "unexpected GDT layout");
    }
    let gdt: &'static GlobalDescriptorTable = gdt;
    gdt.load();
    unsafe {
        CS::set_reg(KERNEL_CS);
        SS::set_reg(KERNEL_DS);
        DS::set_reg(KERNEL_DS);
        ES::set_reg(KERNEL_DS);
        load_tss(TSS);
    }
}
