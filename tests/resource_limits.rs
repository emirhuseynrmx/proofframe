use std::sync::{Arc, Barrier};

use proofframe::{CancellationToken, ErrorCode, ResourceAccount, ResourceLimits};

fn limits(memory: u64, temp: u64) -> ResourceLimits {
    ResourceLimits {
        max_memory_bytes: memory,
        max_temp_bytes: temp,
        max_output_records: 10,
        max_samples: 4,
    }
}

#[test]
fn child_reservations_charge_every_ancestor_and_release_on_drop() {
    let root = ResourceAccount::root(limits(1_024, 0));
    let child = root.child(768, 0);
    let grandchild = child.child(512, 0);

    let held = grandchild.try_reserve_memory(500).unwrap();
    assert_eq!(root.memory_used(), 500);
    assert_eq!(child.memory_used(), 500);
    assert_eq!(grandchild.memory_used(), 500);
    let error = grandchild.try_reserve_memory(13).unwrap_err();
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert_eq!(root.peak_memory_used(), 500);

    drop(held);
    assert_eq!(root.memory_used(), 0);
    assert_eq!(child.memory_used(), 0);
    assert_eq!(grandchild.memory_used(), 0);
}

#[test]
fn siblings_contend_for_the_shared_root_without_overshoot() {
    let root = ResourceAccount::root(limits(1_024, 0));
    let left = root.child(1_024, 0);
    let right = root.child(1_024, 0);
    let held = left.try_reserve_memory(700).unwrap();

    assert_eq!(
        right.try_reserve_memory(400).unwrap_err().code(),
        ErrorCode::ResourceLimit
    );
    assert_eq!(right.memory_used(), 0, "failed reservations must roll back");
    assert_eq!(root.memory_used(), 700);
    drop(held);
}

#[test]
fn overflow_zero_limits_and_temp_budgets_fail_closed() {
    let root = ResourceAccount::root(limits(1_024, 64));
    assert_eq!(
        root.try_reserve_memory(u64::MAX).unwrap_err().code(),
        ErrorCode::ResourceLimit
    );
    let temp = root.try_reserve_temp(64).unwrap();
    assert_eq!(root.temp_used(), 64);
    assert_eq!(root.peak_temp_used(), 64);
    assert_eq!(
        root.try_reserve_temp(1).unwrap_err().code(),
        ErrorCode::ResourceLimit
    );
    drop(temp);
    assert_eq!(root.temp_used(), 0);

    let zero = ResourceAccount::root(limits(0, 0));
    assert_eq!(
        zero.try_reserve_memory(1).unwrap_err().code(),
        ErrorCode::ResourceLimit
    );
    assert_eq!(
        zero.try_reserve_temp(1).unwrap_err().code(),
        ErrorCode::ResourceLimit
    );
}

#[test]
fn concurrent_reservations_never_exceed_the_root_limit() {
    const WORKERS: usize = 8;
    let root = ResourceAccount::root(limits(400, 0));
    let barrier = Arc::new(Barrier::new(WORKERS));
    let handles = (0..WORKERS)
        .map(|_| {
            let account = root.child(400, 0);
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                account.try_reserve_memory(100).ok()
            })
        })
        .collect::<Vec<_>>();
    let reservations = handles
        .into_iter()
        .filter_map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(reservations.len(), 4);
    assert_eq!(root.memory_used(), 400);
    assert_eq!(root.peak_memory_used(), 400);
    drop(reservations);
    assert_eq!(root.memory_used(), 0);
}

#[test]
fn cancellation_is_shared_and_has_a_stable_error_code() {
    let token = CancellationToken::new();
    let observer = token.clone();
    token.check().unwrap();
    token.cancel();

    assert!(observer.is_cancelled());
    assert_eq!(observer.check().unwrap_err().code(), ErrorCode::Cancelled);
}

#[test]
fn public_defaults_are_bounded_and_release_sized() {
    let limits = ResourceLimits::default();
    assert_eq!(limits.max_memory_bytes, 512 * 1024 * 1024);
    assert_eq!(limits.max_temp_bytes, 4 * 1024 * 1024 * 1024);
    assert_eq!(limits.max_output_records, 100_000);
    assert_eq!(limits.max_samples, 100);
}
