//! ELF edits that put NVIDIA's driver into Waydroid's Android 13 (docs/waydroid-nvidia.md).
//!
//! Waydroid's bionic lacks `tools/patches/aosp17-bionic-static-tls-surplus.patch`, so it refuses
//! `R_X86_64_TPOFF64` relocations in `dlopen()`ed libraries ("TLS symbol ... using IE access
//! model"), and it gives static TLS only to libraries loaded at process start. Two edits work
//! around that without rebuilding the image:
//!
//! - [`rewrite_tls`] on NVIDIA's initial-exec TLS libraries: every `R_X86_64_TPOFF64` becomes
//!   `R_X86_64_NONE` with its symbol and addend kept, and `DT_INIT`, `DT_INIT_ARRAY` and
//!   `DT_INIT_ARRAYSZ` are retagged `DT_NVG_*` (bionic ignores unknown tags), so the constructors
//!   wait; `DF_STATIC_TLS` is cleared from `DT_FLAGS` (`DF_SYMBOLIC` stays). nvglibc
//!   (`aosp-device/hals/nvglibc/src/tls_surplus.c`) fills those GOT words with offsets into
//!   `libnvglibc_tls.so`'s static TLS and runs the constructors.
//! - [`add_needed`] on the platform's `libvulkan.so`: `libnvglibc_tls.so` becomes one of its
//!   `DT_NEEDED`, so every process linking libvulkan loads it at start.

use std::fmt;

const DT_NULL: i64 = 0;
const DT_NEEDED: i64 = 1;
const DT_PLTRELSZ: i64 = 2;
const DT_STRTAB: i64 = 5;
const DT_RELA: i64 = 7;
const DT_RELASZ: i64 = 8;
const DT_STRSZ: i64 = 10;
const DT_INIT: i64 = 12;
const DT_SONAME: i64 = 14;
const DT_RPATH: i64 = 15;
const DT_JMPREL: i64 = 23;
const DT_INIT_ARRAY: i64 = 25;
const DT_INIT_ARRAYSZ: i64 = 27;
const DT_RUNPATH: i64 = 29;
const DT_FLAGS: i64 = 30;
const DT_VERDEF: i64 = 0x6fff_fffc;
const DT_VERDEFNUM: i64 = 0x6fff_fffd;
const DT_VERNEED: i64 = 0x6fff_fffe;
const DT_VERNEEDNUM: i64 = 0x6fff_ffff;
const DF_STATIC_TLS: u64 = 0x10;

/// tls_surplus.c's tags, in the OS-specific range.
pub const DT_NVG_INIT: i64 = 0x6e76_6701;
pub const DT_NVG_INIT_ARRAY: i64 = 0x6e76_6702;
pub const DT_NVG_INIT_ARRAYSZ: i64 = 0x6e76_6703;

const R_X86_64_NONE: u32 = 0;
const R_X86_64_TPOFF64: u32 = 18;
const R_X86_64_TPOFF32: u32 = 23;
const R_X86_64_TLSDESC: u32 = 36;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const SHT_DYNSYM: u32 = 11;
const EM_X86_64: u16 = 62;

#[derive(Debug)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error(msg.into()))
}

struct DynEntry {
    at: usize,
    tag: i64,
    val: u64,
}

/// A little-endian ELF64 x86_64 file: bounds-checked field access, the load segments and the
/// dynamic section.
struct Elf<'a> {
    data: &'a mut [u8],
    loads: Vec<(u64, u64, u64)>, // vaddr, offset, filesz
    dynamic: Vec<DynEntry>,
}

impl<'a> Elf<'a> {
    fn parse(data: &'a mut [u8]) -> Result<Self> {
        let mut elf = Elf { data, loads: Vec::new(), dynamic: Vec::new() };
        if elf.bytes::<4>(0)? != *b"\x7fELF" || elf.bytes::<1>(4)?[0] != 2 || elf.u16(18)? != EM_X86_64
        {
            return err("not an x86_64 ELF");
        }
        let phoff = elf.u64(32)? as usize;
        let phentsize = elf.u16(54)? as usize;
        let mut dynamic = None;
        for i in 0..elf.u16(56)? as usize {
            let ph = phoff + i * phentsize;
            let (offset, vaddr, filesz) = (elf.u64(ph + 8)?, elf.u64(ph + 16)?, elf.u64(ph + 32)?);
            match elf.u32(ph)? {
                PT_LOAD => elf.loads.push((vaddr, offset, filesz)),
                PT_DYNAMIC => dynamic = Some((offset as usize, filesz as usize)),
                _ => {}
            }
        }
        let (off, size) = dynamic.ok_or_else(|| Error("no PT_DYNAMIC".into()))?;
        for at in (off..off + size).step_by(16) {
            let tag = elf.u64(at)? as i64;
            if tag == DT_NULL {
                break;
            }
            let val = elf.u64(at + 8)?;
            elf.dynamic.push(DynEntry { at, tag, val });
        }
        Ok(elf)
    }

    fn bytes<const N: usize>(&self, at: usize) -> Result<[u8; N]> {
        self.data
            .get(at..at + N)
            .map(|b| b.try_into().expect("slice of length N"))
            .ok_or_else(|| Error(format!("offset {at:#x} is past the end of the file")))
    }

    fn u16(&self, at: usize) -> Result<u16> {
        Ok(u16::from_le_bytes(self.bytes(at)?))
    }

    fn u32(&self, at: usize) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes(at)?))
    }

    fn u64(&self, at: usize) -> Result<u64> {
        Ok(u64::from_le_bytes(self.bytes(at)?))
    }

    fn put_u64(&mut self, at: usize, v: u64) -> Result<()> {
        self.bytes::<8>(at)?;
        self.data[at..at + 8].copy_from_slice(&v.to_le_bytes());
        Ok(())
    }

    fn value(&self, tag: i64) -> Option<u64> {
        self.dynamic.iter().find(|e| e.tag == tag).map(|e| e.val)
    }

    fn file_offset(&self, vaddr: u64) -> Result<usize> {
        self.loads
            .iter()
            .find(|(v, _, sz)| (*v..v + sz).contains(&vaddr))
            .map(|(v, o, _)| (vaddr - v + o) as usize)
            .ok_or_else(|| Error(format!("address {vaddr:#x} is not in the file")))
    }

    /// The dynamic string table: its file offset and size.
    fn strtab(&self) -> Result<(usize, usize)> {
        match (self.value(DT_STRTAB), self.value(DT_STRSZ)) {
            (Some(addr), Some(size)) => Ok((self.file_offset(addr)?, size as usize)),
            _ => err("no DT_STRTAB"),
        }
    }

    /// The NUL-terminated dynamic string at `name` (an offset into the table).
    fn string(&self, name: u64) -> Result<&[u8]> {
        let (off, size) = self.strtab()?;
        let table = self.data.get(off..off + size).ok_or_else(|| Error("bad DT_STRSZ".into()))?;
        let s = table.get(name as usize..).ok_or_else(|| Error(format!("bad string {name:#x}")))?;
        Ok(&s[..s.iter().position(|&b| b == 0).unwrap_or(s.len())])
    }
}

/// Rewrites a library's initial-exec TLS for nvglibc in place. Returns the number of
/// `TPOFF64` relocations rewritten; with 0 the data is untouched.
pub fn rewrite_tls(data: &mut [u8]) -> Result<usize> {
    let mut elf = Elf::parse(data)?;
    if elf.value(DT_NVG_INIT).is_some() {
        return err("already rewritten");
    }
    let mut count = 0;
    for (table, size) in [(DT_RELA, DT_RELASZ), (DT_JMPREL, DT_PLTRELSZ)] {
        let (Some(addr), Some(size)) = (elf.value(table), elf.value(size)) else { continue };
        let start = elf.file_offset(addr)?;
        for at in (start..start + size as usize).step_by(24) {
            let info = elf.u64(at + 8)?;
            let (sym, rtype) = (info >> 32, info as u32);
            match rtype {
                R_X86_64_TPOFF32 => return err("has R_X86_64_TPOFF32, which nvglibc does not handle"),
                R_X86_64_TLSDESC => return err("has R_X86_64_TLSDESC, which nvglibc does not handle"),
                R_X86_64_TPOFF64 if sym == 0 => return err("R_X86_64_TPOFF64 without a symbol"),
                R_X86_64_TPOFF64 => {
                    elf.put_u64(at + 8, sym << 32 | u64::from(R_X86_64_NONE))?;
                    count += 1;
                }
                _ => {}
            }
        }
    }
    if count == 0 {
        return Ok(0);
    }
    if elf.value(DT_INIT).is_none() {
        return err("no DT_INIT to mark the rewritten library with");
    }
    let edits: Vec<(usize, i64, u64)> = elf.dynamic.iter().map(|e| (e.at, e.tag, e.val)).collect();
    for (at, tag, val) in edits {
        match tag {
            DT_INIT => elf.put_u64(at, DT_NVG_INIT as u64)?,
            DT_INIT_ARRAY => elf.put_u64(at, DT_NVG_INIT_ARRAY as u64)?,
            DT_INIT_ARRAYSZ => elf.put_u64(at, DT_NVG_INIT_ARRAYSZ as u64)?,
            DT_FLAGS => elf.put_u64(at + 8, val & !DF_STATIC_TLS)?,
            _ => {}
        }
    }
    Ok(count)
}

/// Every string-table offset something other than DT_NEEDED points at: the soname and paths, the
/// dynamic symbols' names, and the version definitions and needs.
fn other_string_refs(elf: &Elf) -> Result<Vec<u64>> {
    let mut refs: Vec<u64> = elf
        .dynamic
        .iter()
        .filter(|e| matches!(e.tag, DT_SONAME | DT_RPATH | DT_RUNPATH))
        .map(|e| e.val)
        .collect();

    // .dynsym, found through the section headers (its size is not in the dynamic section)
    let (shoff, shentsize) = (elf.u64(40)? as usize, elf.u16(58)? as usize);
    for i in 0..elf.u16(60)? as usize {
        let sh = shoff + i * shentsize;
        if elf.u32(sh + 4)? != SHT_DYNSYM {
            continue;
        }
        let (off, size, entsize) = (elf.u64(sh + 24)?, elf.u64(sh + 32)?, elf.u64(sh + 56)?);
        for at in (off..off + size).step_by(entsize.max(1) as usize) {
            refs.push(elf.u32(at as usize)?.into());
        }
    }
    // Elf64_Verneed {vn_version, vn_cnt, vn_file, vn_aux, vn_next} and its Elf64_Vernaux
    // {vna_hash, vna_flags, vna_other, vna_name, vna_next}
    if let (Some(addr), Some(num)) = (elf.value(DT_VERNEED), elf.value(DT_VERNEEDNUM)) {
        let mut vn = elf.file_offset(addr)?;
        for _ in 0..num {
            refs.push(elf.u32(vn + 4)?.into());
            let mut aux = vn + elf.u32(vn + 8)? as usize;
            for _ in 0..elf.u16(vn + 2)? {
                refs.push(elf.u32(aux + 8)?.into());
                aux += elf.u32(aux + 12)? as usize;
            }
            vn += elf.u32(vn + 12)? as usize;
        }
    }
    // Elf64_Verdef {vd_version, vd_flags, vd_ndx, vd_cnt, vd_hash, vd_aux, vd_next} and its
    // Elf64_Verdaux {vda_name, vda_next}
    if let (Some(addr), Some(num)) = (elf.value(DT_VERDEF), elf.value(DT_VERDEFNUM)) {
        let mut vd = elf.file_offset(addr)?;
        for _ in 0..num {
            let mut aux = vd + elf.u32(vd + 12)? as usize;
            for _ in 0..elf.u16(vd + 6)? {
                refs.push(elf.u32(aux)?.into());
                aux += elf.u32(aux + 4)? as usize;
            }
            vd += elf.u32(vd + 16)? as usize;
        }
    }
    Ok(refs)
}

/// Makes `name` a DT_NEEDED of the library, in place and without growing the file: the string
/// of a DT_NEEDED library `X` of the same length becomes `name`, so `name` must itself need `X`
/// (the dependency order bionic sees is then `..., name, X, ...`). `X` must not be in the version
/// needs (they name their library) and its string must not be shared with any other name.
/// Returns `X`.
pub fn add_needed(data: &mut [u8], name: &str) -> Result<String> {
    let elf = Elf::parse(data)?;
    let mut needed = Vec::new();
    for e in elf.dynamic.iter().filter(|e| e.tag == DT_NEEDED) {
        needed.push((e.val, String::from_utf8_lossy(elf.string(e.val)?).into_owned()));
    }
    if needed.iter().any(|(_, n)| n == name) {
        return err(format!("already needs {name}"));
    }
    // [start, end] of every other string (end: its NUL), to find one no other name shares
    let mut others = Vec::new();
    for r in other_string_refs(&elf)? {
        others.push((r, r + elf.string(r)?.len() as u64));
    }
    let shared = |off: u64, len: u64| {
        others.iter().any(|&(start, end)| start <= off + len && off <= end)
            || needed.iter().any(|&(o, ref n)| o != off && o <= off + len && off <= o + n.len() as u64)
    };
    let (off, victim) = needed
        .iter()
        .find(|(o, n)| n.len() == name.len() && !shared(*o, n.len() as u64))
        .cloned()
        .ok_or_else(|| {
            Error(format!(
                "no DT_NEEDED library with a {}-byte name of its own to stand in for {name}",
                name.len()
            ))
        })?;
    let (strtab, _) = elf.strtab()?;
    let at = strtab + off as usize;
    drop(elf);
    data[at..at + name.len()].copy_from_slice(name.as_bytes());
    Ok(victim)
}
