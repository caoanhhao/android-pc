//! ELF64 loader for static executables and the initial user stack.
//!
//! Supported: ET_EXEC and static-PIE ET_DYN (loaded at a fixed base)
//! without PT_INTERP. Dynamically linked programs (PT_INTERP, e.g. bionic's
//! /system/bin/linker64) are refused with ENOEXEC and a log line until the
//! interpreter loading lands.

use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::errno::*;
use crate::mm::PAGE_SIZE;
use crate::mm::user::{AddressSpace, PROT_EXEC, PROT_READ, PROT_WRITE, STACK_SIZE, STACK_TOP};

const PT_LOAD: u32 = 1;
const PT_INTERP: u32 = 3;
const PT_PHDR: u32 = 6;
const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
const EM_X86_64: u16 = 62;
const PHDR_SIZE: usize = 56;
/// Load address of static-PIE executables.
const PIE_BASE: u64 = 0x5555_5555_4000;

const AT_NULL: u64 = 0;
const AT_PHDR: u64 = 3;
const AT_PHENT: u64 = 4;
const AT_PHNUM: u64 = 5;
const AT_PAGESZ: u64 = 6;
const AT_BASE: u64 = 7;
const AT_FLAGS: u64 = 8;
const AT_ENTRY: u64 = 9;
const AT_UID: u64 = 11;
const AT_EUID: u64 = 12;
const AT_GID: u64 = 13;
const AT_EGID: u64 = 14;
const AT_PLATFORM: u64 = 15;
const AT_HWCAP: u64 = 16;
const AT_CLKTCK: u64 = 17;
const AT_SECURE: u64 = 23;
const AT_RANDOM: u64 = 25;
const AT_HWCAP2: u64 = 26;
const AT_EXECFN: u64 = 31;
const HWCAP2_FSGSBASE: u64 = 1 << 1;
const AT_MINSIGSTKSZ: u64 = 51;

/// Upper bound on argv + envp bytes (Linux: a quarter of the stack limit).
pub const MAX_ARG_BYTES: usize = 2 << 20;

pub struct Image {
    pub mm: Arc<AddressSpace>,
    pub entry: u64,
    pub sp: u64,
}

struct Phdr {
    ty: u32,
    flags: u32,
    offset: u64,
    vaddr: u64,
    filesz: u64,
    memsz: u64,
}

fn u16_at(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(d[o..o + 2].try_into().unwrap())
}
fn u32_at(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}
fn u64_at(d: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(d[o..o + 8].try_into().unwrap())
}

/// True if `data` starts like an ELF file (used to pick ENOEXEC).
pub fn is_elf(data: &[u8]) -> bool {
    data.len() >= 4 && &data[..4] == b"\x7fELF"
}

pub fn load(data: &[u8], execfn: &[u8], argv: &[Vec<u8>], envp: &[Vec<u8>]) -> KResult<Image> {
    if data.len() < 64 || !is_elf(data) || data[4] != 2 || data[5] != 1 {
        return Err(ENOEXEC);
    }
    let ty = u16_at(data, 16);
    if u16_at(data, 18) != EM_X86_64 || (ty != ET_EXEC && ty != ET_DYN) {
        return Err(ENOEXEC);
    }
    let entry = u64_at(data, 24);
    let phoff = u64_at(data, 32) as usize;
    let phentsize = u16_at(data, 54) as usize;
    let phnum = u16_at(data, 56) as usize;
    if phentsize != PHDR_SIZE || phnum == 0 || phnum > 128 {
        return Err(ENOEXEC);
    }
    let ph_end = phoff.checked_add(phnum * PHDR_SIZE).ok_or(ENOEXEC)?;
    if ph_end > data.len() {
        return Err(ENOEXEC);
    }
    let phdrs: Vec<Phdr> = (0..phnum)
        .map(|i| {
            let o = phoff + i * PHDR_SIZE;
            Phdr {
                ty: u32_at(data, o),
                flags: u32_at(data, o + 4),
                offset: u64_at(data, o + 8),
                vaddr: u64_at(data, o + 16),
                filesz: u64_at(data, o + 32),
                memsz: u64_at(data, o + 40),
            }
        })
        .collect();

    if let Some(interp) = phdrs.iter().find(|p| p.ty == PT_INTERP) {
        let s = data
            .get(interp.offset as usize..(interp.offset + interp.filesz) as usize)
            .unwrap_or(b"?");
        log::warn!(
            "execve {}: dynamically linked (PT_INTERP {}), not supported yet",
            core::str::from_utf8(execfn).unwrap_or("?"),
            core::str::from_utf8(s.strip_suffix(b"\0").unwrap_or(s)).unwrap_or("?")
        );
        return Err(ENOEXEC);
    }
    let bias = if ty == ET_DYN { PIE_BASE } else { 0 };

    let mm = AddressSpace::new().ok_or(ENOMEM)?;
    let mut brk = 0u64;
    let mut phdr_addr = None;
    for p in phdrs.iter().filter(|p| p.ty == PT_LOAD && p.memsz > 0) {
        if p.filesz > p.memsz || p.offset.checked_add(p.filesz).is_none_or(|e| e > data.len() as u64) {
            return Err(ENOEXEC);
        }
        let va = p.vaddr.checked_add(bias).ok_or(ENOEXEC)?;
        let end = va.checked_add(p.memsz).ok_or(ENOEXEC)?;
        if end > crate::mm::user::USER_END || va < PAGE_SIZE {
            return Err(ENOEXEC);
        }
        let mut prot = 0;
        if p.flags & 4 != 0 {
            prot |= PROT_READ;
        }
        if p.flags & 2 != 0 {
            prot |= PROT_WRITE;
        }
        if p.flags & 1 != 0 {
            prot |= PROT_EXEC;
        }
        let start_page = va & !(PAGE_SIZE - 1);
        let end_page = end.next_multiple_of(PAGE_SIZE);
        mm.map_segment(start_page, end_page, prot)?;
        let file = &data[p.offset as usize..(p.offset + p.filesz) as usize];
        mm.copy_out_force(va, file)?;
        // Zero the bss part explicitly: the last file page may be shared.
        let bss = (p.memsz - p.filesz) as usize;
        if bss > 0 {
            let zeros = alloc::vec![0u8; bss.min((end_page - (va + p.filesz)) as usize)];
            mm.copy_out_force(va + p.filesz, &zeros)?;
        }
        brk = brk.max(end_page);
        if p.offset as usize <= phoff && phoff < (p.offset + p.filesz) as usize {
            phdr_addr = Some(va + (phoff as u64 - p.offset));
        }
    }
    if brk == 0 {
        return Err(ENOEXEC);
    }
    if let Some(p) = phdrs.iter().find(|p| p.ty == PT_PHDR) {
        phdr_addr = Some(p.vaddr + bias);
    }
    mm.set_brk_start(brk);
    mm.add_vma(STACK_TOP - STACK_SIZE, STACK_TOP, PROT_READ | PROT_WRITE)?;

    let entry = entry + bias;
    let auxv = [
        (AT_PHDR, phdr_addr.unwrap_or(0)),
        (AT_PHENT, PHDR_SIZE as u64),
        (AT_PHNUM, phnum as u64),
        (AT_PAGESZ, PAGE_SIZE),
        (AT_BASE, 0),
        (AT_FLAGS, 0),
        (AT_ENTRY, entry),
        (AT_UID, 0),
        (AT_EUID, 0),
        (AT_GID, 0),
        (AT_EGID, 0),
        (AT_HWCAP, core::arch::x86_64::__cpuid(1).edx as u64),
        (AT_HWCAP2, if crate::arch::x86_64::cpu::has_fsgsbase() { HWCAP2_FSGSBASE } else { 0 }),
        (AT_CLKTCK, crate::time::HZ),
        (AT_SECURE, 0),
        (AT_MINSIGSTKSZ, crate::process::signal::MINSIGSTKSZ),
    ];
    let sp = build_stack(&mm, execfn, argv, envp, &auxv)?;
    Ok(Image { mm, entry, sp })
}

/// Lay out argc, argv, envp, auxv and the strings they point to below
/// STACK_TOP; returns the initial stack pointer (16-byte aligned, at argc).
fn build_stack(mm: &AddressSpace, execfn: &[u8], argv: &[Vec<u8>], envp: &[Vec<u8>], auxv: &[(u64, u64)]) -> KResult<u64> {
    let mut strings: Vec<u8> = Vec::new();
    let mut p = STACK_TOP - 8;
    let mut push_str = |s: &[u8], strings: &mut Vec<u8>| -> u64 {
        p -= s.len() as u64 + 1;
        let mut v = s.to_vec();
        v.push(0);
        v.extend_from_slice(strings);
        *strings = v;
        p
    };
    let execfn_addr = push_str(execfn, &mut strings);
    let env_addrs: Vec<u64> = envp.iter().rev().map(|e| push_str(e, &mut strings)).collect();
    let arg_addrs: Vec<u64> = argv.iter().rev().map(|a| push_str(a, &mut strings)).collect();
    let platform = push_str(b"x86_64", &mut strings);
    let mut random = [0u8; 16];
    crate::random::fill(&mut random);
    let random_addr = push_str(&random[..15], &mut strings);
    // push_str appended a NUL after 15 bytes: overwrite it with byte 16.
    strings[15] = random[15];
    let strings_start = p;
    mm.copy_out_force(strings_start, &strings)?;

    let mut words: Vec<u64> = Vec::new();
    words.push(argv.len() as u64);
    words.extend(arg_addrs.iter().rev());
    words.push(0);
    words.extend(env_addrs.iter().rev());
    words.push(0);
    for &(k, v) in auxv {
        words.push(k);
        words.push(v);
    }
    words.extend_from_slice(&[AT_RANDOM, random_addr, AT_EXECFN, execfn_addr, AT_PLATFORM, platform, AT_NULL, 0]);
    let sp = (strings_start - words.len() as u64 * 8) & !15;
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    mm.copy_out_force(sp, &bytes)?;
    Ok(sp)
}
