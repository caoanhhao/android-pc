//! Locking primitives on top of `spin`.
//!
//! The kernel is not preemptible: a task only gives up its CPU inside
//! `sched::schedule`, and never while holding a lock. A plain `spin::Mutex`
//! is therefore enough for data that interrupt handlers never touch.
//! Data shared with interrupt handlers (run queue, sleep queue) uses
//! [`IrqLock`], which keeps interrupts off on the local CPU while held.

use core::ops::{Deref, DerefMut};
use spin::{Mutex, MutexGuard};
use x86_64::instructions::interrupts;

pub struct IrqLock<T> {
    inner: Mutex<T>,
}

pub struct IrqGuard<'a, T> {
    guard: Option<MutexGuard<'a, T>>,
    were_enabled: bool,
}

impl<T> IrqLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            inner: Mutex::new(value),
        }
    }

    pub fn lock(&self) -> IrqGuard<'_, T> {
        let were_enabled = interrupts::are_enabled();
        interrupts::disable();
        IrqGuard {
            guard: Some(self.inner.lock()),
            were_enabled,
        }
    }

    /// Release a lock whose guard was forgotten (context switch hand-over).
    ///
    /// # Safety
    /// The lock must be held by the executing CPU with its guard leaked.
    pub unsafe fn force_unlock(&self) {
        unsafe { self.inner.force_unlock() }
    }
}

impl<T> IrqGuard<'_, T> {
    /// Keep the lock held (and interrupts off) past the guard's lifetime;
    /// the new owner releases it with [`IrqLock::force_unlock`].
    pub fn leak(mut self) {
        core::mem::forget(self.guard.take());
    }
}

impl<T> Deref for IrqGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.guard.as_ref().unwrap()
    }
}

impl<T> DerefMut for IrqGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.guard.as_mut().unwrap()
    }
}

impl<T> Drop for IrqGuard<'_, T> {
    fn drop(&mut self) {
        if let Some(g) = self.guard.take() {
            drop(g);
            if self.were_enabled {
                interrupts::enable();
            }
        }
    }
}
