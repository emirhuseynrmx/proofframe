use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::ProofFrameError;

const DEFAULT_MEMORY_BYTES: u64 = 512 * 1024 * 1024;
const DEFAULT_TEMP_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const DEFAULT_OUTPUT_RECORDS: u64 = 100_000;
const DEFAULT_SAMPLES: usize = 100;

/// Hard limits shared by validation, distinct, diff and evidence operations.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub max_memory_bytes: u64,
    pub max_temp_bytes: u64,
    pub max_output_records: u64,
    pub max_samples: usize,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: DEFAULT_MEMORY_BYTES,
            max_temp_bytes: DEFAULT_TEMP_BYTES,
            max_output_records: DEFAULT_OUTPUT_RECORDS,
            max_samples: DEFAULT_SAMPLES,
        }
    }
}

/// Cooperative cancellation shared across cloned operation handles.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub fn check(&self) -> Result<(), ProofFrameError> {
        if self.is_cancelled() {
            Err(ProofFrameError::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug)]
struct Counters {
    memory_cap: u64,
    temp_cap: u64,
    memory: AtomicU64,
    peak_memory: AtomicU64,
    temp: AtomicU64,
    peak_temp: AtomicU64,
    reservation_lock: Mutex<()>,
}

impl Counters {
    fn new(memory_cap: u64, temp_cap: u64) -> Self {
        Self {
            memory_cap,
            temp_cap,
            memory: AtomicU64::new(0),
            peak_memory: AtomicU64::new(0),
            temp: AtomicU64::new(0),
            peak_temp: AtomicU64::new(0),
            reservation_lock: Mutex::new(()),
        }
    }
}

/// A node in a resource hierarchy. Reservations charge this node and every ancestor.
#[derive(Debug, Clone)]
pub struct ResourceAccount {
    nodes: Arc<[Arc<Counters>]>,
    limits: ResourceLimits,
}

impl ResourceAccount {
    #[must_use]
    pub fn root(limits: ResourceLimits) -> Self {
        Self {
            nodes: Arc::from([Arc::new(Counters::new(
                limits.max_memory_bytes,
                limits.max_temp_bytes,
            ))]),
            limits,
        }
    }

    #[must_use]
    pub fn child(&self, memory_cap: u64, temp_cap: u64) -> Self {
        let mut nodes = Vec::with_capacity(self.nodes.len() + 1);
        nodes.extend(self.nodes.iter().cloned());
        nodes.push(Arc::new(Counters::new(memory_cap, temp_cap)));
        Self {
            nodes: nodes.into(),
            limits: self.limits,
        }
    }

    pub fn try_reserve_memory(&self, bytes: u64) -> Result<MemoryReservation, ProofFrameError> {
        Reservation::try_new(self.nodes.clone(), bytes, ResourceKind::Memory).map(|reservation| {
            MemoryReservation {
                _reservation: reservation,
            }
        })
    }

    pub fn try_reserve_temp(&self, bytes: u64) -> Result<TempReservation, ProofFrameError> {
        Reservation::try_new(self.nodes.clone(), bytes, ResourceKind::Temp).map(|reservation| {
            TempReservation {
                _reservation: reservation,
            }
        })
    }

    #[must_use]
    pub fn memory_used(&self) -> u64 {
        self.current().memory.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn peak_memory_used(&self) -> u64 {
        self.current().peak_memory.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn temp_used(&self) -> u64 {
        self.current().temp.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn peak_temp_used(&self) -> u64 {
        self.current().peak_temp.load(Ordering::Acquire)
    }

    #[must_use]
    pub const fn limits(&self) -> ResourceLimits {
        self.limits
    }

    fn current(&self) -> &Counters {
        self.nodes.last().expect("a resource account has a root")
    }
}

#[derive(Debug, Clone, Copy)]
enum ResourceKind {
    Memory,
    Temp,
}

impl ResourceKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Temp => "temporary storage",
        }
    }

    fn used_and_limit(self, counters: &Counters) -> (&AtomicU64, u64) {
        match self {
            Self::Memory => (&counters.memory, counters.memory_cap),
            Self::Temp => (&counters.temp, counters.temp_cap),
        }
    }

    fn peak(self, counters: &Counters) -> &AtomicU64 {
        match self {
            Self::Memory => &counters.peak_memory,
            Self::Temp => &counters.peak_temp,
        }
    }
}

#[derive(Debug)]
struct Reservation {
    nodes: Arc<[Arc<Counters>]>,
    bytes: u64,
    kind: ResourceKind,
}

impl Reservation {
    fn try_new(
        nodes: Arc<[Arc<Counters>]>,
        bytes: u64,
        kind: ResourceKind,
    ) -> Result<Self, ProofFrameError> {
        let transaction_root = Arc::clone(&nodes[0]);
        let _transaction = transaction_root
            .reservation_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut committed_usage = Vec::with_capacity(nodes.len());
        for (charged, counters) in nodes.iter().enumerate() {
            let (used, cap) = kind.used_and_limit(counters);
            let usage = match try_charge(used, cap, bytes) {
                Ok(usage) => usage,
                Err((current, limit)) => {
                    for rollback in &nodes[..charged] {
                        let (used, _) = kind.used_and_limit(rollback);
                        used.fetch_sub(bytes, Ordering::AcqRel);
                    }
                    return Err(ProofFrameError::ResourceLimit {
                        resource: kind.name(),
                        requested: bytes,
                        used: current,
                        limit,
                    });
                }
            };
            committed_usage.push(usage);
        }
        for (counters, usage) in nodes.iter().zip(committed_usage) {
            kind.peak(counters).fetch_max(usage, Ordering::AcqRel);
        }
        Ok(Self { nodes, bytes, kind })
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        for counters in self.nodes.iter() {
            let (used, _) = self.kind.used_and_limit(counters);
            used.fetch_sub(self.bytes, Ordering::AcqRel);
        }
    }
}

/// RAII charge for engine-owned memory.
#[derive(Debug)]
pub struct MemoryReservation {
    _reservation: Reservation,
}

/// RAII charge for operation-owned temporary storage.
#[derive(Debug)]
pub struct TempReservation {
    _reservation: Reservation,
}

fn try_charge(used: &AtomicU64, limit: u64, bytes: u64) -> Result<u64, (u64, u64)> {
    let mut current = used.load(Ordering::Acquire);
    loop {
        let Some(next) = current.checked_add(bytes) else {
            return Err((current, limit));
        };
        if next > limit {
            return Err((current, limit));
        }
        match used.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {
                return Ok(next);
            }
            Err(observed) => current = observed,
        }
    }
}
