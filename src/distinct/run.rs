use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};

use tempfile::NamedTempFile;

use super::{DuplicateSample, ExactSummary, F64Bits, FixedRecord, OwnedValue, ValueKind};
use crate::{CancellationToken, ProofFrameError, ResourceAccount, TempReservation};

const MAGIC: [u8; 8] = *b"PFRUN001";
const VERSION: u16 = 1;
const HEADER_BYTES: u64 = 60;
const MAX_RECORD_BYTES: u64 = 64 * 1024 * 1024;
const VERIFY_BUFFER_BYTES: usize = 4 * 1024;
const MERGE_BUFFER_BYTES: usize = 256;

pub(super) struct RunMeta {
    file: NamedTempFile,
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
    Ok(RunMeta {
        file,
        kind: spec.kind,
        records: spec.records,
        payload_bytes: spec.payload_bytes,
        max_record_bytes: spec.max_record_bytes,
        checksum: spec.checksum,
        _temp: reservation,
    })
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
    let sample_bytes = account
        .limits()
        .max_samples
        .checked_mul(std::mem::size_of::<DuplicateSample>())
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| ProofFrameError::CorruptData("Exact sample size overflowed".into()))?;
    let merge_bytes = cursor_bytes
        .checked_add(retained_value_bytes)
        .and_then(|value| value.checked_add(sample_bytes))
        .ok_or_else(|| ProofFrameError::CorruptData("Exact merge size overflowed".into()))?;
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
    let mut duplicate_samples = Vec::with_capacity(account.limits().max_samples.min(1024));
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
            peak_memory_bytes: account.peak_memory_used(),
            peak_temp_bytes: account.peak_temp_used(),
        },
    })
}

struct RunCursor {
    reader: BufReader<File>,
    kind: ValueKind,
    remaining: u64,
    payload_remaining: u64,
}

impl RunCursor {
    fn open(run: &RunMeta) -> Result<Self, ProofFrameError> {
        let mut file = run.file.reopen()?;
        file.seek(SeekFrom::Start(0))?;
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
    let mut file = run.file.reopen()?;
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
