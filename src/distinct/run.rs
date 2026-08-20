use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};

use tempfile::{NamedTempFile, TempPath};

use super::{DuplicateSample, ExactSummary, F64Bits, FixedRecord, OwnedValue, ValueKind};
use crate::{
    CancellationToken, MemoryReservation, ProofFrameError, ResourceAccount, TempReservation,
};

const MAGIC: [u8; 8] = *b"PFRUN001";
const VERSION: u16 = 1;
const HEADER_BYTES: u64 = 60;
const MAX_RECORD_BYTES: u64 = 64 * 1024 * 1024;
const VERIFY_BUFFER_BYTES: usize = 4 * 1024;
const MERGE_BUFFER_BYTES: usize = 256;
const CHECKSUM_OFFSET: u64 = 28;

pub(super) struct RunMeta {
    path: TempPath,
    kind: ValueKind,
    records: u64,
    payload_bytes: u64,
    max_record_bytes: u64,
    checksum: [u8; 32],
    _temp: TempReservation,
}

impl RunMeta {
    pub(super) fn total_bytes(&self) -> u64 {
        HEADER_BYTES + self.payload_bytes
    }

    fn open(&self) -> Result<File, ProofFrameError> {
        Ok(File::open(&self.path)?)
    }
}

pub(super) trait FixedValue: Copy + Ord {
    const KIND: ValueKind;
    fn write_bytes(self) -> [u8; 8];
    fn read_bytes(bytes: [u8; 8]) -> Self;
    fn into_owned(self) -> OwnedValue;
}

impl FixedValue for i64 {
    const KIND: ValueKind = ValueKind::I64;

    fn write_bytes(self) -> [u8; 8] {
        self.to_le_bytes()
    }

    fn read_bytes(bytes: [u8; 8]) -> Self {
        Self::from_le_bytes(bytes)
    }

    fn into_owned(self) -> OwnedValue {
        OwnedValue::I64(self)
    }
}

impl FixedValue for u64 {
    const KIND: ValueKind = ValueKind::U64;

    fn write_bytes(self) -> [u8; 8] {
        self.to_le_bytes()
    }

    fn read_bytes(bytes: [u8; 8]) -> Self {
        Self::from_le_bytes(bytes)
    }

    fn into_owned(self) -> OwnedValue {
        OwnedValue::U64(self)
    }
}

impl FixedValue for F64Bits {
    const KIND: ValueKind = ValueKind::F64;

    fn write_bytes(self) -> [u8; 8] {
        self.0.to_le_bytes()
    }

    fn read_bytes(bytes: [u8; 8]) -> Self {
        Self(u64::from_le_bytes(bytes))
    }

    fn into_owned(self) -> OwnedValue {
        OwnedValue::F64(self.0)
    }
}

pub(super) fn write_fixed<T: FixedValue>(
    records: &[FixedRecord<T>],
    account: &ResourceAccount,
    directory: &std::path::Path,
) -> Result<RunMeta, ProofFrameError> {
    let record_count = records.len() as u64;
    let payload_bytes = record_count
        .checked_mul(16)
        .ok_or_else(|| ProofFrameError::CorruptData("Exact run size overflowed u64".to_string()))?;
    let mut hasher = blake3::Hasher::new();
    for record in records {
        hasher.update(&record.value.write_bytes());
        hasher.update(&record.row.to_le_bytes());
    }
    let checksum = *hasher.finalize().as_bytes();
    write_run(
        RunSpec {
            kind: T::KIND,
            records: record_count,
            payload_bytes,
            checksum,
            max_record_bytes: 8,
        },
        account,
        directory,
        |writer| {
            for record in records {
                writer.write_all(&record.value.write_bytes())?;
                writer.write_all(&record.row.to_le_bytes())?;
            }
            Ok(())
        },
    )
}

pub(super) fn write_bytes(
    records: &[super::ByteIndex],
    arena: &[u8],
    account: &ResourceAccount,
    directory: &std::path::Path,
) -> Result<RunMeta, ProofFrameError> {
    let payload_bytes = records.iter().try_fold(0_u64, |total, record| {
        total
            .checked_add(12)
            .and_then(|value| value.checked_add(u64::from(record.length)))
            .ok_or_else(|| ProofFrameError::CorruptData("Exact byte run overflowed u64".into()))
    })?;
    let mut hasher = blake3::Hasher::new();
    for record in records {
        let value = record.value(arena);
        hasher.update(&record.length.to_le_bytes());
        hasher.update(value);
        hasher.update(&record.row.to_le_bytes());
    }
    let checksum = *hasher.finalize().as_bytes();
    let max_record_bytes = records
        .iter()
        .map(|record| u64::from(record.length))
        .max()
        .unwrap_or(0);
    write_run(
        RunSpec {
            kind: ValueKind::Bytes,
            records: records.len() as u64,
            payload_bytes,
            checksum,
            max_record_bytes,
        },
        account,
        directory,
        |writer| {
            for record in records {
                writer.write_all(&record.length.to_le_bytes())?;
                writer.write_all(record.value(arena))?;
                writer.write_all(&record.row.to_le_bytes())?;
            }
            Ok(())
        },
    )
}

struct RunSpec {
    kind: ValueKind,
    records: u64,
    payload_bytes: u64,
    checksum: [u8; 32],
    max_record_bytes: u64,
}

fn write_run(
    spec: RunSpec,
    account: &ResourceAccount,
    directory: &std::path::Path,
    write_payload: impl FnOnce(&mut BufWriter<File>) -> Result<(), ProofFrameError>,
) -> Result<RunMeta, ProofFrameError> {
    let total_bytes = HEADER_BYTES
        .checked_add(spec.payload_bytes)
        .ok_or_else(|| ProofFrameError::CorruptData("Exact run size overflowed u64".into()))?;
    let reservation = account.try_reserve_temp(total_bytes)?;
    let file = NamedTempFile::new_in(directory)?;
    let mut writer = BufWriter::new(file.reopen()?);
    writer.write_all(&MAGIC)?;
    writer.write_all(&VERSION.to_le_bytes())?;
    writer.write_all(&[spec.kind.tag(), 0])?;
    writer.write_all(&spec.records.to_le_bytes())?;
    writer.write_all(&spec.payload_bytes.to_le_bytes())?;
    writer.write_all(&spec.checksum)?;
    write_payload(&mut writer)?;
    writer.flush()?;
    writer.get_ref().sync_data()?;
    drop(writer);
    Ok(RunMeta {
        path: file.into_temp_path(),
        kind: spec.kind,
        records: spec.records,
        payload_bytes: spec.payload_bytes,
        max_record_bytes: spec.max_record_bytes,
        checksum: spec.checksum,
        _temp: reservation,
    })
}

pub(super) fn compact(
    runs: &[RunMeta],
    account: &ResourceAccount,
    directory: &std::path::Path,
    cancellation: &CancellationToken,
) -> Result<RunMeta, ProofFrameError> {
    let first = runs
        .first()
        .ok_or_else(|| corrupt("Exact run compaction requires at least one run"))?;
    if runs.iter().any(|run| run.kind != first.kind) {
        return Err(corrupt(
            "Exact run compaction mixed incompatible value kinds",
        ));
    }
    for run in runs {
        verify_payload(run, cancellation)?;
    }

    let records = runs.iter().try_fold(0_u64, |total, run| {
        total
            .checked_add(run.records)
            .ok_or_else(|| corrupt("Exact compacted run record count overflowed u64"))
    })?;
    let payload_bytes = runs.iter().try_fold(0_u64, |total, run| {
        total
            .checked_add(run.payload_bytes)
            .ok_or_else(|| corrupt("Exact compacted run payload overflowed u64"))
    })?;
    let total_bytes = HEADER_BYTES
        .checked_add(payload_bytes)
        .ok_or_else(|| corrupt("Exact compacted run size overflowed u64"))?;
    let max_record_bytes = runs
        .iter()
        .map(|run| run.max_record_bytes)
        .max()
        .unwrap_or(0);
    let reservation = account.try_reserve_temp(total_bytes)?;
    let _merge_memory = account.try_reserve_memory(merge_memory_bytes(runs)?)?;
    let file = NamedTempFile::new_in(directory)?;
    let mut writer = BufWriter::new(file.reopen()?);
    writer.write_all(&MAGIC)?;
    writer.write_all(&VERSION.to_le_bytes())?;
    writer.write_all(&[first.kind.tag(), 0])?;
    writer.write_all(&records.to_le_bytes())?;
    writer.write_all(&payload_bytes.to_le_bytes())?;
    writer.write_all(&[0_u8; 32])?;

    let mut cursors = runs
        .iter()
        .map(RunCursor::open)
        .collect::<Result<Vec<_>, _>>()?;
    let mut heap = BinaryHeap::with_capacity(cursors.len());
    for (run, cursor) in cursors.iter_mut().enumerate() {
        if let Some((value, row)) = cursor.next()? {
            heap.push(Reverse(MergeEntry { value, row, run }));
        }
    }
    let mut hasher = blake3::Hasher::new();
    let mut written_records = 0_u64;
    let mut written_payload = 0_u64;
    while let Some(Reverse(entry)) = heap.pop() {
        if written_records & 0x0fff == 0 {
            cancellation.check()?;
        }
        written_payload = written_payload
            .checked_add(write_owned_record(
                &mut writer,
                &mut hasher,
                &entry.value,
                entry.row,
            )?)
            .ok_or_else(|| corrupt("Exact compacted run payload overflowed u64"))?;
        written_records += 1;
        if let Some((value, row)) = cursors[entry.run].next()? {
            heap.push(Reverse(MergeEntry {
                value,
                row,
                run: entry.run,
            }));
        }
    }
    if written_records != records || written_payload != payload_bytes {
        return Err(corrupt("Exact compacted run length changed during merge"));
    }

    let checksum = *hasher.finalize().as_bytes();
    writer.flush()?;
    writer.seek(SeekFrom::Start(CHECKSUM_OFFSET))?;
    writer.write_all(&checksum)?;
    writer.flush()?;
    writer.get_ref().sync_data()?;
    drop(writer);
    Ok(RunMeta {
        path: file.into_temp_path(),
        kind: first.kind,
        records,
        payload_bytes,
        max_record_bytes,
        checksum,
        _temp: reservation,
    })
}

fn write_owned_record(
    writer: &mut impl Write,
    hasher: &mut blake3::Hasher,
    value: &OwnedValue,
    row: u64,
) -> Result<u64, ProofFrameError> {
    match value {
        OwnedValue::I64(value) => write_fixed_parts(writer, hasher, &value.to_le_bytes(), row),
        OwnedValue::U64(value) | OwnedValue::F64(value) => {
            write_fixed_parts(writer, hasher, &value.to_le_bytes(), row)
        }
        OwnedValue::Bytes(value) => {
            let length = u32::try_from(value.len())
                .map_err(|_| corrupt("Exact compacted byte value exceeds u32"))?;
            let length_bytes = length.to_le_bytes();
            let row_bytes = row.to_le_bytes();
            writer.write_all(&length_bytes)?;
            writer.write_all(value)?;
            writer.write_all(&row_bytes)?;
            hasher.update(&length_bytes);
            hasher.update(value);
            hasher.update(&row_bytes);
            Ok(12 + u64::from(length))
        }
    }
}

fn write_fixed_parts(
    writer: &mut impl Write,
    hasher: &mut blake3::Hasher,
    value: &[u8; 8],
    row: u64,
) -> Result<u64, ProofFrameError> {
    let row = row.to_le_bytes();
    writer.write_all(value)?;
    writer.write_all(&row)?;
    hasher.update(value);
    hasher.update(&row);
    Ok(16)
}

#[derive(Debug, Eq, PartialEq)]
struct MergeEntry {
    value: OwnedValue,
    row: u64,
    run: usize,
}

impl Ord for MergeEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.value
            .cmp(&other.value)
            .then_with(|| self.row.cmp(&other.row))
            .then_with(|| self.run.cmp(&other.run))
    }
}

impl PartialOrd for MergeEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

pub(super) fn merge(
    runs: &[RunMeta],
    account: &ResourceAccount,
    cancellation: &CancellationToken,
) -> Result<ExactSummary, ProofFrameError> {
    for run in runs {
        verify_payload(run, cancellation)?;
    }
    let merge_bytes = merge_memory_bytes(runs)?;
    let _merge_memory = account.try_reserve_memory(merge_bytes)?;
    let mut cursors = runs
        .iter()
        .map(RunCursor::open)
        .collect::<Result<Vec<_>, _>>()?;
    let mut heap = BinaryHeap::with_capacity(cursors.len());
    for (run, cursor) in cursors.iter_mut().enumerate() {
        if let Some((value, row)) = cursor.next()? {
            heap.push(Reverse(MergeEntry { value, row, run }));
        }
    }

    let mut distinct_count = 0_u64;
    let mut duplicate_count = 0_u64;
    let mut duplicate_samples = Vec::with_capacity(account.limits().max_samples);
    let mut previous: Option<(OwnedValue, u64)> = None;
    let mut iterations = 0_u64;
    while let Some(Reverse(entry)) = heap.pop() {
        if iterations & 0x0fff == 0 {
            cancellation.check()?;
        }
        iterations += 1;
        match &previous {
            Some((value, first_row)) if value == &entry.value => {
                duplicate_count += 1;
                if duplicate_samples.len() < account.limits().max_samples {
                    duplicate_samples.push(DuplicateSample {
                        first_row: *first_row,
                        duplicate_row: entry.row,
                    });
                }
            }
            _ => {
                distinct_count += 1;
                previous = Some((entry.value, entry.row));
            }
        }
        if let Some((value, row)) = cursors[entry.run].next()? {
            heap.push(Reverse(MergeEntry {
                value,
                row,
                run: entry.run,
            }));
        }
    }

    Ok(ExactSummary {
        distinct_count,
        duplicate_count,
        duplicate_samples,
        metrics: super::ExactMetrics {
            runs: runs.len() as u64,
            spill_bytes: runs.iter().map(RunMeta::total_bytes).sum(),
            compactions: 0,
            max_merge_fan_in: runs.len() as u64,
            peak_memory_bytes: account.peak_memory_used(),
            peak_temp_bytes: account.peak_temp_used(),
        },
    })
}

pub(super) fn intersect(
    left: &[RunMeta],
    right: &[RunMeta],
    left_account: &ResourceAccount,
    right_account: &ResourceAccount,
    max_samples: usize,
    cancellation: &CancellationToken,
) -> Result<super::IntersectionSummary, ProofFrameError> {
    let sample_bytes = max_samples
        .checked_mul(32)
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or_else(|| ProofFrameError::CorruptData("Leakage sample size overflowed".into()))?;
    let _sample_memory = left_account.try_reserve_memory(sample_bytes)?;
    let mut left = UniqueMerge::new(left, left_account, cancellation)?;
    let mut right = UniqueMerge::new(right, right_account, cancellation)?;
    let mut left_distinct = 0_u64;
    let mut right_distinct = 0_u64;
    let mut overlap = 0_u64;
    let mut samples = Vec::with_capacity(max_samples.min(1024));
    let mut left_value = next_counted(&mut left, &mut left_distinct)?;
    let mut right_value = next_counted(&mut right, &mut right_distinct)?;
    while let (Some(left_item), Some(right_item)) = (&left_value, &right_value) {
        cancellation.check()?;
        match left_item.cmp(right_item) {
            std::cmp::Ordering::Less => {
                left_value = next_counted(&mut left, &mut left_distinct)?;
            }
            std::cmp::Ordering::Greater => {
                right_value = next_counted(&mut right, &mut right_distinct)?;
            }
            std::cmp::Ordering::Equal => {
                overlap += 1;
                if samples.len() < max_samples {
                    let OwnedValue::Bytes(bytes) = left_item else {
                        return Err(corrupt("Leakage intersection requires byte identities"));
                    };
                    let digest: [u8; 32] = bytes
                        .as_ref()
                        .try_into()
                        .map_err(|_| corrupt("Leakage identity digest is not 32 bytes"))?;
                    samples.push(digest);
                }
                left_value = next_counted(&mut left, &mut left_distinct)?;
                right_value = next_counted(&mut right, &mut right_distinct)?;
            }
        }
    }
    while left_value.is_some() {
        left_value = next_counted(&mut left, &mut left_distinct)?;
    }
    while right_value.is_some() {
        right_value = next_counted(&mut right, &mut right_distinct)?;
    }
    Ok(super::IntersectionSummary {
        left_distinct,
        right_distinct,
        overlap,
        samples,
    })
}

fn next_counted(
    merge: &mut UniqueMerge,
    count: &mut u64,
) -> Result<Option<OwnedValue>, ProofFrameError> {
    let value = merge.next_unique()?;
    if value.is_some() {
        *count += 1;
    }
    Ok(value)
}

struct UniqueMerge {
    cursors: Vec<RunCursor>,
    heap: BinaryHeap<Reverse<MergeEntry>>,
    _memory: MemoryReservation,
    cancellation: CancellationToken,
}

impl UniqueMerge {
    fn new(
        runs: &[RunMeta],
        account: &ResourceAccount,
        cancellation: &CancellationToken,
    ) -> Result<Self, ProofFrameError> {
        for run in runs {
            verify_payload(run, cancellation)?;
        }
        let memory = account.try_reserve_memory(merge_memory_bytes(runs)?)?;
        let mut cursors = runs
            .iter()
            .map(RunCursor::open)
            .collect::<Result<Vec<_>, _>>()?;
        let mut heap = BinaryHeap::with_capacity(cursors.len());
        for (run, cursor) in cursors.iter_mut().enumerate() {
            if let Some((value, row)) = cursor.next()? {
                heap.push(Reverse(MergeEntry { value, row, run }));
            }
        }
        Ok(Self {
            cursors,
            heap,
            _memory: memory,
            cancellation: cancellation.clone(),
        })
    }

    fn next_unique(&mut self) -> Result<Option<OwnedValue>, ProofFrameError> {
        self.cancellation.check()?;
        let Some(Reverse(first)) = self.heap.pop() else {
            return Ok(None);
        };
        let value = first.value;
        self.refill(first.run)?;
        loop {
            let Some(Reverse(candidate)) = self.heap.peek() else {
                break;
            };
            if candidate.value != value {
                break;
            }
            let Reverse(duplicate) = self.heap.pop().expect("heap was just inspected");
            self.refill(duplicate.run)?;
        }
        Ok(Some(value))
    }

    fn refill(&mut self, run: usize) -> Result<(), ProofFrameError> {
        if let Some((value, row)) = self.cursors[run].next()? {
            self.heap.push(Reverse(MergeEntry { value, row, run }));
        }
        Ok(())
    }
}

fn merge_memory_bytes(runs: &[RunMeta]) -> Result<u64, ProofFrameError> {
    let cursor_bytes = runs.iter().try_fold(0_u64, |total, run| {
        let fixed = MERGE_BUFFER_BYTES
            .checked_add(std::mem::size_of::<RunCursor>())
            .and_then(|value| value.checked_add(std::mem::size_of::<Reverse<MergeEntry>>()))
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| ProofFrameError::CorruptData("Exact merge size overflowed".into()))?;
        total
            .checked_add(fixed)
            .and_then(|value| value.checked_add(run.max_record_bytes))
            .ok_or_else(|| ProofFrameError::CorruptData("Exact merge size overflowed".into()))
    })?;
    let retained_value_bytes = runs
        .iter()
        .map(|run| run.max_record_bytes)
        .max()
        .unwrap_or(0);
    cursor_bytes
        .checked_add(retained_value_bytes)
        .ok_or_else(|| ProofFrameError::CorruptData("Exact merge size overflowed".into()))
}

struct RunCursor {
    reader: BufReader<File>,
    kind: ValueKind,
    remaining: u64,
    payload_remaining: u64,
}

impl RunCursor {
    fn open(run: &RunMeta) -> Result<Self, ProofFrameError> {
        let mut file = run.open()?;
        file.rewind()?;
        if file.metadata()?.len() != HEADER_BYTES + run.payload_bytes {
            return Err(corrupt("Exact run length does not match its header"));
        }
        let mut reader = BufReader::with_capacity(MERGE_BUFFER_BYTES, file);
        let mut magic = [0_u8; 8];
        read_exact_partition(&mut reader, &mut magic)?;
        if magic != MAGIC {
            return Err(corrupt("Exact run magic is invalid"));
        }
        let version = read_u16(&mut reader)?;
        if version != VERSION {
            return Err(corrupt("Exact run version is unsupported"));
        }
        let mut kind_bytes = [0_u8; 2];
        read_exact_partition(&mut reader, &mut kind_bytes)?;
        let kind = ValueKind::from_tag(kind_bytes[0])?;
        let records = read_u64(&mut reader)?;
        let payload_bytes = read_u64(&mut reader)?;
        let mut checksum = [0_u8; 32];
        read_exact_partition(&mut reader, &mut checksum)?;
        if kind != run.kind
            || records != run.records
            || payload_bytes != run.payload_bytes
            || checksum != run.checksum
        {
            return Err(corrupt("Exact run metadata changed after creation"));
        }
        Ok(Self {
            reader,
            kind,
            remaining: records,
            payload_remaining: payload_bytes,
        })
    }

    fn next(&mut self) -> Result<Option<(OwnedValue, u64)>, ProofFrameError> {
        if self.remaining == 0 {
            self.verify_end()?;
            return Ok(None);
        }
        let value = match self.kind {
            ValueKind::I64 => {
                let bytes = self.read_payload_array::<8>()?;
                i64::read_bytes(bytes).into_owned()
            }
            ValueKind::U64 => {
                let bytes = self.read_payload_array::<8>()?;
                u64::read_bytes(bytes).into_owned()
            }
            ValueKind::F64 => {
                let bytes = self.read_payload_array::<8>()?;
                F64Bits::read_bytes(bytes).into_owned()
            }
            ValueKind::Bytes => {
                let length = u64::from(u32::from_le_bytes(self.read_payload_array::<4>()?));
                if length > MAX_RECORD_BYTES || length > self.payload_remaining.saturating_sub(8) {
                    return Err(corrupt("Exact run byte length exceeds its bounded payload"));
                }
                let length = usize::try_from(length)
                    .map_err(|_| corrupt("Exact run byte length exceeds usize"))?;
                let mut bytes = vec![0_u8; length];
                self.read_payload(&mut bytes)?;
                OwnedValue::Bytes(bytes.into_boxed_slice())
            }
        };
        let row = u64::from_le_bytes(self.read_payload_array::<8>()?);
        self.remaining -= 1;
        Ok(Some((value, row)))
    }

    fn read_payload_array<const N: usize>(&mut self) -> Result<[u8; N], ProofFrameError> {
        let mut bytes = [0_u8; N];
        self.read_payload(&mut bytes)?;
        Ok(bytes)
    }

    fn read_payload(&mut self, bytes: &mut [u8]) -> Result<(), ProofFrameError> {
        let length = bytes.len() as u64;
        if length > self.payload_remaining {
            return Err(corrupt("Exact run record crosses the payload boundary"));
        }
        read_exact_partition(&mut self.reader, bytes)?;
        self.payload_remaining -= length;
        Ok(())
    }

    fn verify_end(&mut self) -> Result<(), ProofFrameError> {
        if self.payload_remaining != 0 {
            return Err(corrupt(
                "Exact run record count does not consume its payload",
            ));
        }
        let mut trailing = [0_u8; 1];
        if self.reader.read(&mut trailing)? != 0 {
            return Err(corrupt("Exact run contains trailing bytes"));
        }
        Ok(())
    }
}

fn verify_payload(run: &RunMeta, cancellation: &CancellationToken) -> Result<(), ProofFrameError> {
    let mut file = run.open()?;
    if file.metadata()?.len() != HEADER_BYTES + run.payload_bytes {
        return Err(corrupt("Exact run length does not match its header"));
    }
    file.seek(SeekFrom::Start(HEADER_BYTES))?;
    let mut remaining = run.payload_bytes;
    let mut buffer = [0_u8; VERIFY_BUFFER_BYTES];
    let mut hasher = blake3::Hasher::new();
    while remaining != 0 {
        cancellation.check()?;
        let length = usize::try_from(remaining.min(buffer.len() as u64))
            .expect("bounded verification length fits usize");
        read_exact_partition(&mut file, &mut buffer[..length])?;
        hasher.update(&buffer[..length]);
        remaining -= length as u64;
    }
    if hasher.finalize().as_bytes() != &run.checksum {
        return Err(corrupt("Exact run payload checksum is invalid"));
    }
    Ok(())
}

fn read_u16(reader: &mut impl Read) -> Result<u16, ProofFrameError> {
    let mut bytes = [0_u8; 2];
    read_exact_partition(reader, &mut bytes)?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u64(reader: &mut impl Read) -> Result<u64, ProofFrameError> {
    let mut bytes = [0_u8; 8];
    read_exact_partition(reader, &mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn read_exact_partition(reader: &mut impl Read, bytes: &mut [u8]) -> Result<(), ProofFrameError> {
    reader
        .read_exact(bytes)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::UnexpectedEof => corrupt("Exact run is truncated"),
            _ => ProofFrameError::Io(error),
        })
}

fn corrupt(message: &str) -> ProofFrameError {
    ProofFrameError::CorruptData(message.to_string())
}
