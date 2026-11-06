use std::sync::{Arc, Condvar, Mutex, RwLock};

use super::{CondvarUnpoisoned, LockUnpoisoned, RwLockUnpoisoned};

#[test]
fn a_poisoned_mutex_still_hands_out_its_value() {
    let shared = Arc::new(Mutex::new(1));
    let held = Arc::clone(&shared);
    let _ = std::thread::spawn(move || {
        let _guard = held.lock_unpoisoned();
        panic!("poison the lock");
    })
    .join();
    assert!(shared.is_poisoned());
    *shared.lock_unpoisoned() += 1;
    assert_eq!(*shared.lock_unpoisoned(), 2);
}

#[test]
fn a_poisoned_rwlock_still_reads_and_writes() {
    let shared = Arc::new(RwLock::new(1));
    let held = Arc::clone(&shared);
    let _ = std::thread::spawn(move || {
        let _guard = held.write_unpoisoned();
        panic!("poison the lock");
    })
    .join();
    assert!(shared.is_poisoned());
    *shared.write_unpoisoned() += 1;
    assert_eq!(*shared.read_unpoisoned(), 2);
}

#[test]
fn a_condvar_wait_on_a_poisoned_mutex_still_hands_back_the_guard() {
    let shared = Arc::new((Mutex::new(false), Condvar::new()));
    let held = Arc::clone(&shared);
    let _ = std::thread::spawn(move || {
        let mut ready = held.0.lock_unpoisoned();
        *ready = true;
        held.1.notify_all();
        panic!("poison the lock");
    })
    .join();
    let (lock, changed) = &*shared;
    let mut ready = lock.lock_unpoisoned();
    while !*ready {
        ready = changed.wait_unpoisoned(ready);
    }
    assert!(lock.is_poisoned());
}
