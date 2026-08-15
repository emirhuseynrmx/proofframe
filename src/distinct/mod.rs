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
    directory: PathBuf,
    cancellation: CancellationToken,
    _sample_memory: MemoryReservation,
}

impl ExactState {
    pub fn new(
        kind: ValueKind,
        account: ResourceAccount,
        directory: PathBuf,
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

    pub fn new_with_cancellation(
        kind: ValueKind,
        account: ResourceAccount,
        directory: PathBuf,
        row_count_hint: Option<u64>,
        cancellation: CancellationToken,
    ) -> Result<Self, ProofFrameError> {
        if !directory.is_dir() {
            return Err(ProofFrameError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Exact-state temporary directory does not exist",
            )));
        }
        let hinted = row_count_hint.and_then(|rows| usize::try_from(rows).ok());
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
            _sample_memory: sample_memory,
        })
    }

    pub fn insert(&mut self, value: ValueRef<'_>, row: u64) -> Result<(), ProofFrameError> {
        match (&mut self.storage, value) {
            (Storage::I64(store), ValueRef::I64(value)) => insert_fixed(
                store,
                value,
                row,
                &self.account,
                &self.directory,
                &mut self.runs,
            ),
            (Storage::U64(store), ValueRef::U64(value)) => insert_fixed(
                store,
                value,
                row,
                &self.account,
                &self.directory,
                &mut self.runs,
            ),
            (Storage::F64(store), ValueRef::F64(value)) => insert_fixed(
                store,
                F64Bits(value),
                row,
                &self.account,
                &self.directory,
                &mut self.runs,
            ),
            (Storage::Bytes(store), ValueRef::Bytes(value)) => insert_bytes(
                store,
                value,
                row,
                &self.account,
                &self.directory,
                &mut self.runs,
            ),
            _ => Err(ProofFrameError::CorruptData(
                "Value kind does not match the exact-state plan".into(),
            )),
        }
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
        run::merge(&self.runs, &self.account, &self.cancellation)
    }

    fn seal(&mut self) -> Result<(), ProofFrameError> {
        self.cancellation.check()?;
        match &mut self.storage {
            Storage::I64(store) => {
                spill_fixed(store, &self.account, &self.directory, &mut self.runs)?
            }
            Storage::U64(store) => {
                spill_fixed(store, &self.account, &self.directory, &mut self.runs)?
            }
            Storage::F64(store) => {
                spill_fixed(store, &self.account, &self.directory, &mut self.runs)?
            }
            Storage::Bytes(store) => {
                spill_bytes(store, &self.account, &self.directory, &mut self.runs)?
            }
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
    _memory: MemoryReservation,
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
    directory: &std::path::Path,
    runs: &mut Vec<RunMeta>,
) -> Result<(), ProofFrameError> {
    if store
        .segment
        .as_ref()
        .is_some_and(|segment| segment.records.len() == segment.capacity)
    {
        spill_fixed(store, account, directory, runs)?;
    }
    if store.segment.is_none() {
        store.segment = Some(allocate_fixed_segment(store.desired_capacity, account)?);
    }
    store
        .segment
        .as_mut()
        .expect("segment was allocated")
        .records
        .push(FixedRecord { value, row });
    Ok(())
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
                    _memory: memory,
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
    _memory: MemoryReservation,
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
    directory: &std::path::Path,
    runs: &mut Vec<RunMeta>,
) -> Result<(), ProofFrameError> {
    let value_length = u32::try_from(value.len()).map_err(|_| ProofFrameError::ResourceLimit {
        resource: "single exact value",
        requested: value.len() as u64,
        used: 0,
        limit: u64::from(u32::MAX),
    })?;
    let full = store.segment.as_ref().is_some_and(|segment| {
        segment.records.len() == segment.record_capacity
            || value.len() > segment.byte_capacity - segment.arena.len()
    });
    if full {
        spill_bytes(store, account, directory, runs)?;
    }
    if store.segment.is_none() {
        store.segment = Some(allocate_byte_segment(
            store.desired_records,
            value.len(),
            account,
        )?);
    }
    let segment = store.segment.as_mut().expect("segment was allocated");
    let offset = u32::try_from(segment.arena.len()).expect("byte capacity fits u32");
    segment.arena.extend_from_slice(value);
    segment.records.push(ByteIndex {
        offset,
        length: value_length,
        row,
    });
    Ok(())
}

fn allocate_byte_segment(
    desired_records: usize,
    minimum_bytes: usize,
    account: &ResourceAccount,
) -> Result<ByteSegment, ProofFrameError> {
    let mut record_capacity = desired_records;
    let mut byte_capacity = desired_records
        .saturating_mul(ASSUMED_BYTES_PER_VALUE)
        .max(minimum_bytes)
        .min(u32::MAX as usize);
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
                    _memory: memory,
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
