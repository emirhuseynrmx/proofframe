//! Bounded exact distinct and uniqueness state backed by sorted temporary runs.

mod run;

use std::path::PathBuf;

use crate::{CancellationToken, MemoryReservation, ProofFrameError, ResourceAccount};
use run::{FixedValue, RunMeta};

const DEFAULT_FIXED_RECORDS_PER_SEGMENT: usize = 65_536;
const MAX_FIXED_RECORDS_PER_SEGMENT: usize = 1 << 25;
const DEFAULT_BYTE_RECORDS_PER_SEGMENT: usize = 16_384;
const MAX_BYTE_RECORDS_PER_SEGMENT: usize = 1 << 24;
const ASSUMED_BYTES_PER_VALUE: usize = 32;
const MAX_MERGE_FAN_IN: usize = 32;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ValueKind {
    I64,
    U64,
    F64,
    Bytes,
}

impl ValueKind {
    pub(crate) const fn tag(self) -> u8 {
        match self {
            Self::I64 => 1,
            Self::U64 => 2,
            Self::F64 => 3,
            Self::Bytes => 4,
        }
    }

    pub(crate) fn from_tag(tag: u8) -> Result<Self, ProofFrameError> {
        match tag {
            1 => Ok(Self::I64),
            2 => Ok(Self::U64),
            3 => Ok(Self::F64),
            4 => Ok(Self::Bytes),
            _ => Err(ProofFrameError::CorruptData(
                "Exact run value kind is invalid".into(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ValueRef<'a> {
    I64(i64),
    U64(u64),
    F64(u64),
    Bytes(&'a [u8]),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DuplicateSample {
    pub first_row: u64,
    pub duplicate_row: u64,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct ExactMetrics {
    pub runs: u64,
    pub spill_bytes: u64,
    pub compactions: u64,
    pub max_merge_fan_in: u64,
    pub peak_memory_bytes: u64,
    pub peak_temp_bytes: u64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ExactSummary {
    pub distinct_count: u64,
    pub duplicate_count: u64,
    pub duplicate_samples: Vec<DuplicateSample>,
    pub metrics: ExactMetrics,
}

pub(crate) struct IntersectionSummary {
    pub(crate) left_distinct: u64,
    pub(crate) right_distinct: u64,
    pub(crate) overlap: u64,
    pub(crate) samples: Vec<[u8; 32]>,
}

/// The first row that carried a left key with no counterpart on the right.
///
/// Only the row is retained. Findings for exact set rules point at the offending row rather
/// than rendering the value, so nothing has to hold a decoded copy of the key.
pub(crate) struct MissingSample {
    pub(crate) row: u64,
}

pub(crate) struct AntiJoinSummary {
    pub(crate) left_distinct: u64,
    pub(crate) right_distinct: u64,
    pub(crate) missing: u64,
    pub(crate) samples: Vec<MissingSample>,
    pub(crate) spill_bytes: u64,
    pub(crate) runs: u64,
}

#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum OwnedValue {
    I64(i64),
    U64(u64),
    F64(u64),
    Bytes(Box<[u8]>),
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct F64Bits(pub(super) u64);

pub struct ExactState {
    storage: Storage,
    runs: Vec<RunMeta>,
    account: ResourceAccount,
    directory: Option<PathBuf>,
    cancellation: CancellationToken,
    compactions: u64,
    max_merge_fan_in: u64,
    _sample_memory: MemoryReservation,
}

/// The directory to spill into, or the reason there is nothing to spill into.
///
/// Exhausting an in-memory budget with no spill target is a resource limit, not a
/// missing file: the caller asked for exactness without giving the engine anywhere
/// to put the overflow, and the honest answer is to stop rather than to guess.
fn spill_target<'a>(
    directory: Option<&'a PathBuf>,
    account: &ResourceAccount,
    held: u64,
) -> Result<&'a PathBuf, ProofFrameError> {
    directory.ok_or(ProofFrameError::ResourceLimit {
        resource: "exact_state_values_without_spill",
        requested: 1,
        used: held,
        limit: account.limits().max_memory_bytes,
    })
}

impl ExactState {
    /// `directory` takes a `PathBuf` or an `Option<PathBuf>`; `None` means there is
    /// nowhere to spill to, so the scan runs in memory and fails closed on the budget.
    pub fn new(
        kind: ValueKind,
        account: ResourceAccount,
        directory: impl Into<Option<PathBuf>>,
        row_count_hint: Option<u64>,
    ) -> Result<Self, ProofFrameError> {
        Self::new_with_cancellation(
            kind,
            account,
            directory,
            row_count_hint,
            CancellationToken::new(),
        )
    }

    /// Build exact state, spilling into `directory` when the segment fills.
    ///
    /// `None` means there is nowhere to spill to: a read-only filesystem, a
    /// locked-down container, or WebAssembly, which has no filesystem at all.
    /// The scan then runs entirely in memory and fails closed the moment the
    /// budget cannot hold the next value, rather than requiring a writable
    /// directory to check uniqueness on three rows.
    pub fn new_with_cancellation(
        kind: ValueKind,
        account: ResourceAccount,
        directory: impl Into<Option<PathBuf>>,
        row_count_hint: Option<u64>,
        cancellation: CancellationToken,
    ) -> Result<Self, ProofFrameError> {
        let directory = directory.into();
        if directory.as_ref().is_some_and(|path| !path.is_dir()) {
            return Err(ProofFrameError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Exact-state temporary directory does not exist",
            )));
        }
        // With a spill target the segment is sized to the file, so a scan that fits
        // writes one run and no more. Without one there is nothing to size against:
        // sizing from the budget made the first column of a scan swallow it and
        // every later column fail on its first value. The segment now starts at the
        // default and grows while the budget allows, which is the only bound left.
        let hinted = directory
            .is_some()
            .then(|| row_count_hint.and_then(|rows| usize::try_from(rows).ok()))
            .flatten();
        let storage = match kind {
            ValueKind::I64 => Storage::I64(FixedStore::new(
                hinted.unwrap_or(DEFAULT_FIXED_RECORDS_PER_SEGMENT),
            )),
            ValueKind::U64 => Storage::U64(FixedStore::new(
                hinted.unwrap_or(DEFAULT_FIXED_RECORDS_PER_SEGMENT),
            )),
            ValueKind::F64 => Storage::F64(FixedStore::new(
                hinted.unwrap_or(DEFAULT_FIXED_RECORDS_PER_SEGMENT),
            )),
            ValueKind::Bytes => Storage::Bytes(ByteStore::new(
                hinted.unwrap_or(DEFAULT_BYTE_RECORDS_PER_SEGMENT),
            )),
        };
        let sample_bytes = account
            .limits()
            .max_samples
            .checked_mul(std::mem::size_of::<DuplicateSample>())
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or_else(|| ProofFrameError::CorruptData("Exact sample size overflowed".into()))?;
        let sample_memory = account.try_reserve_memory(sample_bytes)?;
        Ok(Self {
            storage,
            runs: Vec::new(),
            account,
            directory,
            cancellation,
            compactions: 0,
            max_merge_fan_in: 0,
            _sample_memory: sample_memory,
        })
    }

    pub fn insert(&mut self, value: ValueRef<'_>, row: u64) -> Result<(), ProofFrameError> {
        let needs_spill = match (&self.storage, value) {
            (Storage::I64(store), ValueRef::I64(_)) => fixed_store_is_full(store),
            (Storage::U64(store), ValueRef::U64(_)) => fixed_store_is_full(store),
            (Storage::F64(store), ValueRef::F64(_)) => fixed_store_is_full(store),
            (Storage::Bytes(store), ValueRef::Bytes(value)) => byte_store_is_full(store, value),
            _ => {
                return Err(ProofFrameError::CorruptData(
                    "Value kind does not match the exact-state plan".into(),
                ));
            }
        };
        if needs_spill {
            if self.directory.is_none() {
                // The kinds were already reconciled above, so the value has nothing
                // left to say here except how long it is, and only the byte store
                // has a use for that.
                let length = if let ValueRef::Bytes(bytes) = value {
                    bytes.len()
                } else {
                    0
                };
                match &mut self.storage {
                    Storage::I64(store) => grow_fixed(store, &self.account)?,
                    Storage::U64(store) => grow_fixed(store, &self.account)?,
                    Storage::F64(store) => grow_fixed(store, &self.account)?,
                    Storage::Bytes(store) => grow_bytes(store, length, &self.account)?,
                }
            } else {
                self.spill_current()?;
                self.compact_runs_if_needed()?;
            }
        }
        match (&mut self.storage, value) {
            (Storage::I64(store), ValueRef::I64(value)) => {
                insert_fixed(store, value, row, &self.account)
            }
            (Storage::U64(store), ValueRef::U64(value)) => {
                insert_fixed(store, value, row, &self.account)
            }
            (Storage::F64(store), ValueRef::F64(value)) => {
                insert_fixed(store, F64Bits(value), row, &self.account)
            }
            (Storage::Bytes(store), ValueRef::Bytes(value)) => {
                insert_bytes(store, value, row, &self.account)
            }
            _ => Err(ProofFrameError::CorruptData(
                "Value kind does not match the exact-state plan".into(),
            )),
        }?;
        self.compact_runs_if_needed()
    }

    pub fn finish(mut self) -> Result<ExactSummary, ProofFrameError> {
        if self.runs.is_empty() {
            let max_samples = self.account.limits().max_samples;
            return match self.storage {
                Storage::I64(segment) => finish_fixed_in_memory(
                    segment.segment,
                    &self.account,
                    max_samples,
                    &self.cancellation,
                ),
                Storage::U64(segment) => finish_fixed_in_memory(
                    segment.segment,
                    &self.account,
                    max_samples,
                    &self.cancellation,
                ),
                Storage::F64(segment) => finish_fixed_in_memory(
                    segment.segment,
                    &self.account,
                    max_samples,
                    &self.cancellation,
                ),
                Storage::Bytes(segment) => finish_bytes_in_memory(
                    segment.segment,
                    &self.account,
                    max_samples,
                    &self.cancellation,
                ),
            };
        }
        self.seal()?;
        let mut summary = run::merge(&self.runs, &self.account, &self.cancellation)?;
        summary.metrics.compactions = self.compactions;
        summary.metrics.max_merge_fan_in = self.max_merge_fan_in.max(self.runs.len() as u64);
        Ok(summary)
    }

    fn seal(&mut self) -> Result<(), ProofFrameError> {
        self.cancellation.check()?;
        self.spill_current()?;
        self.compact_runs_if_needed()
    }

    fn spill_current(&mut self) -> Result<(), ProofFrameError> {
        let held = match &self.storage {
            Storage::I64(store) => store.segment.as_ref().map_or(0, |s| s.records.len() as u64),
            Storage::U64(store) => store.segment.as_ref().map_or(0, |s| s.records.len() as u64),
            Storage::F64(store) => store.segment.as_ref().map_or(0, |s| s.records.len() as u64),
            Storage::Bytes(store) => store.segment.as_ref().map_or(0, |s| s.records.len() as u64),
        };
        let directory = spill_target(self.directory.as_ref(), &self.account, held)?;
        match &mut self.storage {
            Storage::I64(store) => spill_fixed(store, &self.account, directory, &mut self.runs)?,
            Storage::U64(store) => spill_fixed(store, &self.account, directory, &mut self.runs)?,
            Storage::F64(store) => spill_fixed(store, &self.account, directory, &mut self.runs)?,
            Storage::Bytes(store) => spill_bytes(store, &self.account, directory, &mut self.runs)?,
        }
        Ok(())
    }

    fn compact_runs_if_needed(&mut self) -> Result<(), ProofFrameError> {
        while self.runs.len() > MAX_MERGE_FAN_IN {
            self.cancellation.check()?;
            let inputs = self.runs.drain(..MAX_MERGE_FAN_IN).collect::<Vec<_>>();
            let compacted = run::compact(
                &inputs,
                &self.account,
                spill_target(self.directory.as_ref(), &self.account, 0)?,
                &self.cancellation,
            )?;
            self.max_merge_fan_in = self
                .max_merge_fan_in
                .max(u64::try_from(inputs.len()).unwrap_or(u64::MAX));
            self.compactions = self.compactions.saturating_add(1);
            self.runs.insert(0, compacted);
        }
        Ok(())
    }
}

pub(crate) fn intersect_exact_states(
    mut left: ExactState,
    mut right: ExactState,
    max_samples: usize,
) -> Result<IntersectionSummary, ProofFrameError> {
    left.seal()?;
    right.seal()?;
    run::intersect(
        &left.runs,
        &right.runs,
        &left.account,
        &right.account,
        max_samples,
        &left.cancellation,
    )
}

/// A finished state kept readable so more than one anti-join can probe it.
///
/// Sealing writes every buffered value into sorted runs; the runs are then immutable, so
/// a reference dataset is scanned once even when several partitions are checked against it.
pub(crate) struct SealedState {
    runs: Vec<RunMeta>,
    account: ResourceAccount,
    // Each run owns its own temporary file, but they were created inside this directory
    // and it has to outlive them.
    _directory: tempfile::TempDir,
}

impl SealedState {
    pub(crate) fn seal(
        mut state: ExactState,
        directory: tempfile::TempDir,
    ) -> Result<Self, ProofFrameError> {
        state.seal()?;
        Ok(Self {
            runs: std::mem::take(&mut state.runs),
            account: state.account.clone(),
            _directory: directory,
        })
    }
}

/// Report the distinct keys in `left` that `right` does not contain.
pub(crate) fn anti_join_exact_states(
    mut left: ExactState,
    right: &SealedState,
    max_samples: usize,
) -> Result<AntiJoinSummary, ProofFrameError> {
    left.seal()?;
    // Read the spill shape before the merge borrows the runs; the caller reports it as
    // execution metrics the same way a unique or composite state does.
    let spill_bytes = left
        .runs
        .iter()
        .fold(0_u64, |total, run| total.saturating_add(run.total_bytes()));
    let runs = left.runs.len() as u64;
    let mut summary = run::anti_join(
        &left.runs,
        &right.runs,
        &left.account,
        &right.account,
        max_samples,
        &left.cancellation,
    )?;
    summary.spill_bytes = spill_bytes;
    summary.runs = runs;
    Ok(summary)
}

enum Storage {
    I64(FixedStore<i64>),
    U64(FixedStore<u64>),
    F64(FixedStore<F64Bits>),
    Bytes(ByteStore),
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FixedRecord<T> {
    pub(super) value: T,
    pub(super) row: u64,
}

struct FixedSegment<T> {
    records: Vec<FixedRecord<T>>,
    capacity: usize,
    // One reservation per enlargement. A segment that grows keeps every charge it
    // was granted, so releasing it releases all of them.
    _memory: Vec<MemoryReservation>,
}

struct FixedStore<T> {
    segment: Option<FixedSegment<T>>,
    desired_capacity: usize,
}

impl<T> FixedStore<T> {
    fn new(hinted: usize) -> Self {
        Self {
            segment: None,
            desired_capacity: hinted.clamp(1, MAX_FIXED_RECORDS_PER_SEGMENT),
        }
    }
}

fn insert_fixed<T: FixedValue>(
    store: &mut FixedStore<T>,
    value: T,
    row: u64,
    account: &ResourceAccount,
) -> Result<(), ProofFrameError> {
    if store.segment.is_none() {
        store.segment = Some(allocate_fixed_segment(store.desired_capacity, account)?);
    }
    let segment = store.segment.as_mut().expect("segment was allocated");
    debug_assert!(segment.records.len() < segment.capacity);
    segment.records.push(FixedRecord { value, row });
    Ok(())
}

/// Enlarge the in-memory segment rather than spilling it.
///
/// Spilling is how a scan stays inside a small budget when it has somewhere to put
/// the overflow. With nowhere to put it, a full segment is not a reason to stop:
/// the only real bound is the memory budget, and the account enforces that on
/// every allocation. So the segment doubles and the account decides when to say no,
/// with numbers that describe the memory rather than a segment size nobody chose.
fn grow_fixed<T>(
    store: &mut FixedStore<T>,
    account: &ResourceAccount,
) -> Result<(), ProofFrameError> {
    let Some(segment) = store.segment.as_mut() else {
        return Ok(());
    };
    let extra = segment.capacity.max(1);
    let bytes = extra
        .checked_mul(std::mem::size_of::<FixedRecord<T>>())
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| ProofFrameError::CorruptData("Exact segment size overflowed".into()))?;
    // Charged before the allocation, so the budget is what refuses rather than the
    // allocator. Reallocating holds the old buffer alongside the new one for the
    // length of the move; the charge covers the larger of the two.
    let memory = account.try_reserve_memory(bytes)?;
    segment.records.reserve_exact(extra);
    segment.capacity += extra;
    segment._memory.push(memory);
    Ok(())
}

/// The same, for values whose size is not known in advance.
///
/// Either the index or the arena can be what filled up, so only the one that did
/// is enlarged, and the arena is never grown past the range a `u32` offset can
/// address — that bound belongs to the run format rather than to a budget.
fn grow_bytes(
    store: &mut ByteStore,
    value_length: usize,
    account: &ResourceAccount,
) -> Result<(), ProofFrameError> {
    let Some(segment) = store.segment.as_mut() else {
        return Ok(());
    };
    let extra_records = if segment.records.len() == segment.record_capacity {
        segment.record_capacity.max(1)
    } else {
        0
    };
    let free = segment.byte_capacity - segment.arena.len();
    let extra_bytes = if value_length > free {
        segment.byte_capacity.max(value_length - free).max(1)
    } else {
        0
    };
    let grown = segment
        .byte_capacity
        .checked_add(extra_bytes)
        .filter(|total| *total <= u32::MAX as usize)
        .ok_or(ProofFrameError::ResourceLimit {
            resource: "exact_state_arena",
            requested: extra_bytes as u64,
            used: segment.byte_capacity as u64,
            limit: u64::from(u32::MAX),
        })?;
    let index_bytes = extra_records
        .checked_mul(std::mem::size_of::<ByteIndex>())
        .ok_or_else(|| ProofFrameError::CorruptData("Exact byte index overflowed".into()))?;
    let bytes = index_bytes
        .checked_add(extra_bytes)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| ProofFrameError::CorruptData("Exact byte arena overflowed".into()))?;
    let memory = account.try_reserve_memory(bytes)?;
    segment.records.reserve_exact(extra_records);
    segment.arena.reserve_exact(extra_bytes);
    segment.record_capacity += extra_records;
    segment.byte_capacity = grown;
    segment._memory.push(memory);
    Ok(())
}

fn fixed_store_is_full<T>(store: &FixedStore<T>) -> bool {
    store
        .segment
        .as_ref()
        .is_some_and(|segment| segment.records.len() == segment.capacity)
}

fn allocate_fixed_segment<T>(
    desired_capacity: usize,
    account: &ResourceAccount,
) -> Result<FixedSegment<T>, ProofFrameError> {
    let mut capacity = desired_capacity;
    loop {
        let bytes = capacity
            .checked_mul(std::mem::size_of::<FixedRecord<T>>())
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| ProofFrameError::CorruptData("Exact segment size overflowed".into()))?;
        match account.try_reserve_memory(bytes) {
            Ok(memory) => {
                return Ok(FixedSegment {
                    records: Vec::with_capacity(capacity),
                    capacity,
                    _memory: vec![memory],
                });
            }
            Err(error) if capacity > 1 && error.code() == crate::ErrorCode::ResourceLimit => {
                capacity = (capacity / 2).max(1);
            }
            Err(error) => return Err(error),
        }
    }
}

fn spill_fixed<T: FixedValue>(
    store: &mut FixedStore<T>,
    account: &ResourceAccount,
    directory: &std::path::Path,
    runs: &mut Vec<RunMeta>,
) -> Result<(), ProofFrameError> {
    let Some(mut segment) = store.segment.take() else {
        return Ok(());
    };
    if segment.records.is_empty() {
        return Ok(());
    }
    segment.records.sort_unstable_by(|left, right| {
        left.value.cmp(&right.value).then(left.row.cmp(&right.row))
    });
    let run = run::write_fixed(&segment.records, account, directory)?;
    runs.push(run);
    Ok(())
}

#[repr(C)]
pub(super) struct ByteIndex {
    offset: u32,
    pub(super) length: u32,
    pub(super) row: u64,
}

impl ByteIndex {
    pub(super) fn value<'a>(&self, arena: &'a [u8]) -> &'a [u8] {
        let start = self.offset as usize;
        &arena[start..start + self.length as usize]
    }
}

struct ByteSegment {
    arena: Vec<u8>,
    records: Vec<ByteIndex>,
    record_capacity: usize,
    byte_capacity: usize,
    _memory: Vec<MemoryReservation>,
}

struct ByteStore {
    segment: Option<ByteSegment>,
    desired_records: usize,
}

impl ByteStore {
    fn new(hinted: usize) -> Self {
        Self {
            segment: None,
            desired_records: hinted.clamp(1, MAX_BYTE_RECORDS_PER_SEGMENT),
        }
    }
}

fn finish_fixed_in_memory<T: FixedValue>(
    segment: Option<FixedSegment<T>>,
    account: &ResourceAccount,
    max_samples: usize,
    cancellation: &CancellationToken,
) -> Result<ExactSummary, ProofFrameError> {
    let Some(mut segment) = segment else {
        return Ok(empty_summary(account));
    };
    segment.records.sort_unstable_by(|left, right| {
        left.value.cmp(&right.value).then(left.row.cmp(&right.row))
    });
    let mut distinct_count = 0_u64;
    let mut duplicate_count = 0_u64;
    let mut duplicate_samples = Vec::with_capacity(max_samples);
    let mut previous: Option<usize> = None;
    for index in 0..segment.records.len() {
        if index & 0x0fff == 0 {
            cancellation.check()?;
        }
        let record = &segment.records[index];
        let duplicate_of = previous
            .filter(|previous_index| segment.records[*previous_index].value == record.value);
        if let Some(previous_index) = duplicate_of {
            duplicate_count += 1;
            if duplicate_samples.len() < max_samples {
                duplicate_samples.push(DuplicateSample {
                    first_row: segment.records[previous_index].row,
                    duplicate_row: record.row,
                });
            }
        } else {
            distinct_count += 1;
            previous = Some(index);
        }
    }
    Ok(ExactSummary {
        distinct_count,
        duplicate_count,
        duplicate_samples,
        metrics: ExactMetrics {
            peak_memory_bytes: account.peak_memory_used(),
            peak_temp_bytes: account.peak_temp_used(),
            ..ExactMetrics::default()
        },
    })
}

fn finish_bytes_in_memory(
    segment: Option<ByteSegment>,
    account: &ResourceAccount,
    max_samples: usize,
    cancellation: &CancellationToken,
) -> Result<ExactSummary, ProofFrameError> {
    let Some(mut segment) = segment else {
        return Ok(empty_summary(account));
    };
    let arena = &segment.arena;
    segment.records.sort_unstable_by(|left, right| {
        left.value(arena)
            .cmp(right.value(arena))
            .then(left.row.cmp(&right.row))
    });
    let mut distinct_count = 0_u64;
    let mut duplicate_count = 0_u64;
    let mut duplicate_samples = Vec::with_capacity(max_samples);
    let mut previous: Option<usize> = None;
    for index in 0..segment.records.len() {
        if index & 0x0fff == 0 {
            cancellation.check()?;
        }
        let record = &segment.records[index];
        let duplicate_of = previous.filter(|previous_index| {
            segment.records[*previous_index].value(arena) == record.value(arena)
        });
        if let Some(previous_index) = duplicate_of {
            duplicate_count += 1;
            if duplicate_samples.len() < max_samples {
                duplicate_samples.push(DuplicateSample {
                    first_row: segment.records[previous_index].row,
                    duplicate_row: record.row,
                });
            }
        } else {
            distinct_count += 1;
            previous = Some(index);
        }
    }
    Ok(ExactSummary {
        distinct_count,
        duplicate_count,
        duplicate_samples,
        metrics: ExactMetrics {
            peak_memory_bytes: account.peak_memory_used(),
            peak_temp_bytes: account.peak_temp_used(),
            ..ExactMetrics::default()
        },
    })
}

fn empty_summary(account: &ResourceAccount) -> ExactSummary {
    ExactSummary {
        distinct_count: 0,
        duplicate_count: 0,
        duplicate_samples: Vec::new(),
        metrics: ExactMetrics {
            peak_memory_bytes: account.peak_memory_used(),
            peak_temp_bytes: account.peak_temp_used(),
            ..ExactMetrics::default()
        },
    }
}

fn insert_bytes(
    store: &mut ByteStore,
    value: &[u8],
    row: u64,
    account: &ResourceAccount,
) -> Result<(), ProofFrameError> {
    let value_length = u32::try_from(value.len()).map_err(|_| ProofFrameError::ResourceLimit {
        resource: "single exact value",
        requested: value.len() as u64,
        used: 0,
        limit: u64::from(u32::MAX),
    })?;
    if store.segment.is_none() {
        store.segment = Some(allocate_byte_segment(
            store.desired_records,
            value.len(),
            account,
        )?);
    }
    let segment = store.segment.as_mut().expect("segment was allocated");
    debug_assert!(segment.records.len() < segment.record_capacity);
    debug_assert!(value.len() <= segment.byte_capacity - segment.arena.len());
    let offset = u32::try_from(segment.arena.len()).expect("byte capacity fits u32");
    segment.arena.extend_from_slice(value);
    segment.records.push(ByteIndex {
        offset,
        length: value_length,
        row,
    });
    Ok(())
}

fn byte_store_is_full(store: &ByteStore, value: &[u8]) -> bool {
    store.segment.as_ref().is_some_and(|segment| {
        segment.records.len() == segment.record_capacity
            || value.len() > segment.byte_capacity - segment.arena.len()
    })
}

fn allocate_byte_segment(
    desired_records: usize,
    minimum_bytes: usize,
    account: &ResourceAccount,
) -> Result<ByteSegment, ProofFrameError> {
    let mut record_capacity = desired_records;
    let mut byte_capacity = desired_records
        .saturating_mul(ASSUMED_BYTES_PER_VALUE)
        .clamp(minimum_bytes, u32::MAX as usize);
    loop {
        let index_bytes = record_capacity
            .checked_mul(std::mem::size_of::<ByteIndex>())
            .ok_or_else(|| ProofFrameError::CorruptData("Exact byte index overflowed".into()))?;
        let total = index_bytes
            .checked_add(byte_capacity)
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| ProofFrameError::CorruptData("Exact byte arena overflowed".into()))?;
        match account.try_reserve_memory(total) {
            Ok(memory) => {
                return Ok(ByteSegment {
                    arena: Vec::with_capacity(byte_capacity),
                    records: Vec::with_capacity(record_capacity),
                    record_capacity,
                    byte_capacity,
                    _memory: vec![memory],
                });
            }
            Err(error)
                if error.code() == crate::ErrorCode::ResourceLimit
                    && (record_capacity > 1 || byte_capacity > minimum_bytes) =>
            {
                record_capacity = (record_capacity / 2).max(1);
                byte_capacity = (byte_capacity / 2).max(minimum_bytes);
            }
            Err(error) => return Err(error),
        }
    }
}

fn spill_bytes(
    store: &mut ByteStore,
    account: &ResourceAccount,
    directory: &std::path::Path,
    runs: &mut Vec<RunMeta>,
) -> Result<(), ProofFrameError> {
    let Some(mut segment) = store.segment.take() else {
        return Ok(());
    };
    if segment.records.is_empty() {
        return Ok(());
    }
    let arena = &segment.arena;
    segment.records.sort_unstable_by(|left, right| {
        left.value(arena)
            .cmp(right.value(arena))
            .then(left.row.cmp(&right.row))
    });
    let run = run::write_bytes(&segment.records, &segment.arena, account, directory)?;
    runs.push(run);
    Ok(())
}
