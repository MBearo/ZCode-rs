//! 锁的取用（审查 X8）：中毒的锁照样取内层，全工程一种写法。
//!
//! 中毒时继续用内层，是原来每一处各自的选择（`.lock().unwrap_or_else(PoisonError::into_inner)`）；这里只统一写法，
//! 不改语义。以前约 100 个文件里手写 197 处（含 `RwLock` 与 `get_mut`），另有 13 个只转发的本地 `fn lock`（已删）。
//! 门禁：产品代码里 `PoisonError::into_inner` 只许在本文件（`scripts/rust-cli/boundaries/policy.mjs` 的
//! `SINGLE_DEFINITIONS`，按代码查）。

use std::sync::{
    Condvar, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard,
};

/// `Mutex` 的取用：中毒时取内层。
pub trait LockUnpoisoned<T: ?Sized> {
    fn lock_unpoisoned(&self) -> MutexGuard<'_, T>;
    /// 独占借用时直接取内层（不加锁）。
    fn get_mut_unpoisoned(&mut self) -> &mut T;
}

impl<T: ?Sized> LockUnpoisoned<T> for Mutex<T> {
    fn lock_unpoisoned(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn get_mut_unpoisoned(&mut self) -> &mut T {
        self.get_mut().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `RwLock` 的取用：中毒时取内层。
pub trait RwLockUnpoisoned<T: ?Sized> {
    fn read_unpoisoned(&self) -> RwLockReadGuard<'_, T>;
    fn write_unpoisoned(&self) -> RwLockWriteGuard<'_, T>;
}

impl<T: ?Sized> RwLockUnpoisoned<T> for RwLock<T> {
    fn read_unpoisoned(&self) -> RwLockReadGuard<'_, T> {
        self.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write_unpoisoned(&self) -> RwLockWriteGuard<'_, T> {
        self.write().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `Condvar` 的等待：醒来时锁中毒也取回内层（同 [`LockUnpoisoned`]）。
pub trait CondvarUnpoisoned {
    fn wait_unpoisoned<'a, T>(&self, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T>;
}

impl CondvarUnpoisoned for Condvar {
    fn wait_unpoisoned<'a, T>(&self, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        self.wait(guard).unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
#[path = "sync_tests.rs"]
mod tests;
