use std::sync::{Arc, Mutex, RwLock};

use super::{LockUnpoisoned, RwLockUnpoisoned};

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
