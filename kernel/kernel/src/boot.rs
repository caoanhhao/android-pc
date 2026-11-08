//! Limine boot protocol requests and accessors for the responses.
//!
//! All responses live in bootloader-reclaimable memory, reachable through the
//! HHDM. That memory must not be reused until everything needed from it has
//! been copied out (not done yet: the frame allocator only uses USABLE memory).

use limine::BaseRevision;
use limine::memmap;
use limine::mp::MpRespData;
use limine::request::{
    BootloaderInfoRequest, ExecutableAddressRequest, ExecutableCmdlineRequest, HhdmRequest,
    MemmapRequest, MpRequest, RsdpRequest, StackSizeRequest,
};

/// Lowest base revision whose semantics this kernel relies on (HHDM covers
/// only memory-map entries; no identity map of low memory).
const MIN_BASE_REVISION: u64 = 3;

/// BSP boot stack size requested from Limine.
const BOOT_STACK_SIZE: u64 = 256 * 1024;

#[used]
#[unsafe(link_section = ".requests_start_marker")]
static REQUESTS_START: limine::RequestsStartMarker = limine::RequestsStartMarker::new();

#[used]
#[unsafe(link_section = ".requests")]
static BASE_REVISION: BaseRevision = BaseRevision::new();

#[used]
#[unsafe(link_section = ".requests")]
static BOOTLOADER_INFO: BootloaderInfoRequest = BootloaderInfoRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static STACK_SIZE: StackSizeRequest = StackSizeRequest::new(BOOT_STACK_SIZE);

#[used]
#[unsafe(link_section = ".requests")]
static HHDM: HhdmRequest = HhdmRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static MEMMAP: MemmapRequest = MemmapRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static EXECUTABLE_ADDRESS: ExecutableAddressRequest = ExecutableAddressRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static CMDLINE: ExecutableCmdlineRequest = ExecutableCmdlineRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static RSDP: RsdpRequest = RsdpRequest::new();

/// Ask Limine to enable x2APIC when the CPU supports it.
#[used]
#[unsafe(link_section = ".requests")]
static MP: MpRequest = MpRequest::new(limine::mp::MP_FLAG_X2APIC);

#[used]
#[unsafe(link_section = ".requests_end_marker")]
static REQUESTS_END: limine::RequestsEndMarker = limine::RequestsEndMarker::new();

/// Validate the boot protocol handshake and log what the bootloader gave us.
pub fn init() {
    let revision = BASE_REVISION.actual_revision();
    match revision {
        Some(r) if r >= MIN_BASE_REVISION => {}
        other => panic!(
            "Limine base revision {other:?} unsupported (need >= {MIN_BASE_REVISION}); \
             update the bootloader"
        ),
    }
    if let Some(info) = BOOTLOADER_INFO.response() {
        log::info!(
            "bootloader: {} {} (base revision {})",
            info.name(),
            info.version(),
            revision.unwrap()
        );
    }
    if STACK_SIZE.response().is_none() {
        log::warn!("bootloader ignored the stack size request");
    }
    if let Some(cmdline) = CMDLINE.response()
        && !cmdline.cmdline().is_empty() {
            log::info!("cmdline: {}", cmdline.cmdline());
        }
}

pub fn hhdm_offset() -> u64 {
    HHDM.response().expect("no HHDM response").offset
}

pub fn memory_map() -> &'static [&'static memmap::Entry] {
    MEMMAP.response().expect("no memory map response").entries()
}

/// (physical base, virtual base) of the loaded kernel image.
pub fn kernel_address() -> (u64, u64) {
    let r = EXECUTABLE_ADDRESS
        .response()
        .expect("no executable address response");
    (r.physical_base, r.virtual_base)
}

pub fn mp() -> Option<&'static MpRespData> {
    MP.response().map(|r| &**r)
}

/// ACPI RSDP address as handed over by Limine (needed later for ACPI/MADT).
#[allow(dead_code)]
pub fn rsdp() -> Option<u64> {
    RSDP.response().map(|r| r.address as u64)
}

pub fn memmap_type_name(t: u64) -> &'static str {
    match t {
        memmap::MEMMAP_USABLE => "usable",
        memmap::MEMMAP_RESERVED => "reserved",
        memmap::MEMMAP_ACPI_RECLAIMABLE => "ACPI reclaimable",
        memmap::MEMMAP_ACPI_NVS => "ACPI NVS",
        memmap::MEMMAP_BAD_MEMORY => "bad memory",
        memmap::MEMMAP_BOOTLOADER_RECLAIMABLE => "bootloader reclaimable",
        memmap::MEMMAP_EXECUTABLE_AND_MODULES => "kernel and modules",
        memmap::MEMMAP_FRAMEBUFFER => "framebuffer",
        memmap::MEMMAP_MAPPED_RESERVED => "mapped reserved",
        _ => "unknown",
    }
}
