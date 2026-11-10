//! Framebuffer console: the kernel log on the boot framebuffer (UEFI GOP
//! or VBE), for machines without a serial port.
//!
//! Everything printed to the console ([`crate::kprint`]: the kernel log,
//! /dev/kmsg lines from userspace, panics) also goes to a text grid in
//! RAM, drawn with the 8x16 VGA font ([`super::font`], scaled 2x or 3x on
//! wide screens) in the log level's color (from the ANSI escapes the
//! logger writes). Scrolling moves the grid half a screen and redraws it
//! from RAM, so the framebuffer is only written, never read (it may be
//! uncached video memory).
//!
//! The console draws until the DRM driver shows its first frame on the
//! boot framebuffer (simpledrm, [`release`]); after that the grid keeps
//! filling in RAM, and the Pause/Break key of a USB keyboard toggles it
//! back over Android's picture ([`toggle_overlay`]: scanout is held while
//! it shows) and away again. A kernel panic always draws.

use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use super::font;

const MAX_COLS: usize = 400;
const MAX_ROWS: usize = 200;

/// Foreground colors (0xRRGGBB) by ANSI code: default, red, green,
/// yellow, cyan, gray, white.
const PALETTE: [u32; 7] = [0xc0c0c0, 0xff5555, 0x55ff55, 0xffff55, 0x55ffff, 0x808080, 0xffffff];

#[derive(Clone, Copy)]
struct Cell {
    ch: u8,
    color: u8,
}

const BLANK: Cell = Cell { ch: b' ', color: 0 };

struct Con {
    /// Framebuffer through the direct map, 32 bpp.
    fb: u64,
    width: usize,
    height: usize,
    pitch: usize,
    shifts: (u8, u8, u8),
    scale: usize,
    cols: usize,
    rows: usize,
    x: usize,
    y: usize,
    color: u8,
    /// Inside an escape sequence: the number being read, or None after
    /// ESC alone.
    esc: Option<Option<u32>>,
    cells: [[Cell; MAX_COLS]; MAX_ROWS],
}

static CON: Mutex<Con> = Mutex::new(Con {
    fb: 0,
    width: 0,
    height: 0,
    pitch: 0,
    shifts: (16, 8, 0),
    scale: 1,
    cols: 0,
    rows: 0,
    x: 0,
    y: 0,
    color: 0,
    esc: None,
    cells: [[BLANK; MAX_COLS]; MAX_ROWS],
});

/// A framebuffer exists and the grid is set up.
static PRESENT: AtomicBool = AtomicBool::new(false);
/// Characters are drawn (not only kept in the grid).
static DRAW: AtomicBool = AtomicBool::new(false);
/// The DRM driver owns the framebuffer.
static HANDED_OVER: AtomicBool = AtomicBool::new(false);

impl Con {
    fn rgb(&self, c: u32) -> u32 {
        let (r, g, b) = (c >> 16 & 0xff, c >> 8 & 0xff, c & 0xff);
        r << self.shifts.0 | g << self.shifts.1 | b << self.shifts.2
    }

    fn draw_cell(&self, cx: usize, cy: usize) {
        let cell = self.cells[cy][cx];
        let fg = self.rgb(PALETTE[cell.color as usize % PALETTE.len()]);
        let glyph = match cell.ch {
            0x20..=0x7e => &font::GLYPHS[(cell.ch - 0x20) as usize],
            _ => &font::GLYPHS[(b'?' - 0x20) as usize],
        };
        let s = self.scale;
        let (px, py) = (cx * font::WIDTH * s, cy * font::HEIGHT * s);
        for (r, bits) in glyph.iter().enumerate() {
            for sy in 0..s {
                let row = (self.fb + ((py + r * s + sy) * self.pitch) as u64) as *mut u32;
                for c in 0..font::WIDTH {
                    let v = if bits & (0x80 >> c) != 0 { fg } else { 0 };
                    for sx in 0..s {
                        // SAFETY: inside the framebuffer (rows and cols fit it).
                        unsafe { core::ptr::write_volatile(row.add(px + c * s + sx), v) };
                    }
                }
            }
        }
    }

    fn redraw(&self) {
        for y in 0..self.rows {
            for x in 0..self.cols {
                self.draw_cell(x, y);
            }
        }
        // The margins the grid does not cover (over a DRM picture).
        let (cw, ch) = (self.cols * font::WIDTH * self.scale, self.rows * font::HEIGHT * self.scale);
        for y in 0..self.height {
            let row = (self.fb + (y * self.pitch) as u64) as *mut u32;
            let from = if y < ch { cw } else { 0 };
            for x in from..self.width {
                // SAFETY: inside the framebuffer.
                unsafe { core::ptr::write_volatile(row.add(x), 0) };
            }
        }
    }

    fn newline(&mut self) {
        self.x = 0;
        self.y += 1;
        if self.y < self.rows {
            return;
        }
        // Scroll half a screen: one full redraw per rows/2 lines.
        let half = (self.rows / 2).max(1);
        self.cells.copy_within(half..self.rows, 0);
        for row in &mut self.cells[self.rows - half..self.rows] {
            *row = [BLANK; MAX_COLS];
        }
        self.y = self.rows - half;
        if DRAW.load(Ordering::Relaxed) {
            self.redraw();
        }
    }

    fn put(&mut self, b: u8) {
        if let Some(num) = self.esc {
            match b {
                b'[' if num.is_none() => self.esc = Some(Some(0)),
                b'0'..=b'9' => self.esc = Some(Some(num.unwrap_or(0) * 10 + (b - b'0') as u32)),
                b';' => {
                    self.sgr(num.unwrap_or(0));
                    self.esc = Some(Some(0));
                }
                _ => {
                    if b == b'm' {
                        self.sgr(num.unwrap_or(0));
                    }
                    self.esc = None;
                }
            }
            return;
        }
        match b {
            0x1b => self.esc = Some(None),
            b'\n' => self.newline(),
            b'\r' => self.x = 0,
            b'\t' => {
                for _ in 0..8 - self.x % 8 {
                    self.put(b' ');
                }
            }
            _ => {
                if self.x >= self.cols {
                    self.newline();
                }
                self.cells[self.y][self.x] = Cell { ch: b, color: self.color };
                if DRAW.load(Ordering::Relaxed) {
                    self.draw_cell(self.x, self.y);
                }
                self.x += 1;
            }
        }
    }

    /// Select Graphic Rendition: the colors the logger uses.
    fn sgr(&mut self, n: u32) {
        self.color = match n {
            0 | 39 => 0,
            31 => 1,
            32 => 2,
            33 => 3,
            36 => 4,
            90 => 5,
            1 | 37 | 97 => 6,
            _ => self.color,
        };
    }
}

impl Write for Con {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            self.put(b);
        }
        Ok(())
    }
}

/// Set up the console on the boot framebuffer (32 bpp only), before
/// anything is logged.
pub fn init() {
    let Some(fb) = crate::boot::framebuffer() else { return };
    if fb.bpp != 32 || fb.width < 320 || fb.height < 200 {
        return;
    }
    let mut c = CON.lock();
    c.fb = crate::boot::hhdm_offset() + fb.phys;
    c.pitch = fb.pitch as usize;
    c.width = fb.width as usize;
    c.height = fb.height as usize;
    c.shifts = fb.shifts;
    c.scale = (fb.width as usize / 1280).clamp(1, 3);
    c.cols = (fb.width as usize / (font::WIDTH * c.scale)).min(MAX_COLS);
    c.rows = (fb.height as usize / (font::HEIGHT * c.scale)).min(MAX_ROWS);
    DRAW.store(true, Ordering::Relaxed);
    c.redraw();
    PRESENT.store(true, Ordering::Release);
    let _ = writeln!(c, "\x1b[97mandroid-pc kernel log\x1b[0m ({}x{}; after Android shows its picture, Pause/Break toggles this log)", fb.width, fb.height);
}

/// Print to the console (called by [`super::serial::_print`] with
/// interrupts off).
pub fn write_fmt(args: fmt::Arguments) {
    if PRESENT.load(Ordering::Acquire) {
        let _ = CON.lock().write_fmt(args);
    }
}

/// The DRM driver starts showing frames on the framebuffer: stop drawing
/// (unless the log is toggled on).
pub fn release() {
    if PRESENT.load(Ordering::Acquire) && !HANDED_OVER.swap(true, Ordering::AcqRel) {
        DRAW.store(false, Ordering::Release);
        log::info!("fbcon: the display belongs to DRM now (Pause/Break shows this log)");
    }
}

/// Whether DRM should hold its scanout because the log is shown.
pub fn holds_display() -> bool {
    HANDED_OVER.load(Ordering::Acquire) && DRAW.load(Ordering::Acquire)
}

/// Show or hide the log over the DRM picture (Pause/Break).
pub fn toggle_overlay() {
    if !PRESENT.load(Ordering::Acquire) || !HANDED_OVER.load(Ordering::Acquire) {
        return;
    }
    let show = !DRAW.load(Ordering::Acquire);
    x86_64::instructions::interrupts::without_interrupts(|| {
        let Some(c) = CON.try_lock() else { return };
        DRAW.store(show, Ordering::Release);
        if show {
            c.redraw();
        }
    });
    if !show {
        super::drm::redraw_boot_framebuffer();
    }
}

/// From the panic handler: take the console whatever state it is in.
///
/// # Safety
/// Only on the panic path (the lock may be held by the interrupted code).
pub unsafe fn panic_takeover() {
    if !PRESENT.load(Ordering::Acquire) {
        return;
    }
    unsafe { CON.force_unlock() };
    if !DRAW.swap(true, Ordering::AcqRel) {
        CON.lock().redraw();
    }
}
