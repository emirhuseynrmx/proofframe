use std::fs::File;
use std::io::{BufReader, BufWriter, Cursor, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::ProofFrameError;

pub(super) const PARTITIONS: usize = 64;
pub(super) const WRITER_BUFFER_BYTES: usize = 8 * 1024;
const MAGIC: [u8; 8] = *b"PFPART02";
const VERSION: u16 = 2;
const HEADER_BYTES: u16 = 92;
#[cfg(feature = "fuzzing")]
const MAX_FUZZ_INPUT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub(super) struct PartitionLimits {
    pub max_record_bytes: u64,
    pub max_columns: u32,
}

#[derive(Debug)]
pub(super) struct RowEntry {
    pub display_key: String,
    pub values: Vec<Option<Vec<u8>>>,
    pub hash: [u8; 32],
}

pub(super) struct PartitionWriter {
    writer: BufWriter<File>,
    schema_digest: [u8; 32],
    records: u64,
    payload_bytes: u64,
    hasher: blake3::Hasher,
    scratch: Vec<u8>,
}

impl PartitionWriter {
    pub(super) fn create(path: &Path, schema_digest: [u8; 32]) -> Result<Self, ProofFrameError> {
        let mut writer = BufWriter::with_capacity(WRITER_BUFFER_BYTES, File::create(path)?);
        writer.write_all(&[0_u8; HEADER_BYTES as usize])?;
        Ok(Self {
            writer,
            schema_digest,
            records: 0,
            payload_bytes: 0,
            hasher: blake3::Hasher::new(),
            scratch: Vec::new(),
        })
    }

    pub(super) fn write_record(
        &mut self,
        key: &[u8],
        entry: &RowEntry,
        reserve_temp: impl FnOnce(u64) -> Result<(), ProofFrameError>,
    ) -> Result<(), ProofFrameError> {
        self.scratch.clear();
        encode_bytes(&mut self.scratch, key)?;
        encode_bytes(&mut self.scratch, entry.display_key.as_bytes())?;
        self.scratch.extend_from_slice(&entry.hash);
        let value_count = u32::try_from(entry.values.len())
            .map_err(|_| corrupt("Diff row has more than u32 columns"))?;
        self.scratch.extend_from_slice(&value_count.to_le_bytes());
        for value in &entry.values {
            match value {
                Some(bytes) => encode_bytes(&mut self.scratch, bytes)?,
                None => self.scratch.extend_from_slice(&u64::MAX.to_le_bytes()),
            }
        }
        let record_bytes = u64::try_from(self.scratch.len())
            .map_err(|_| corrupt("Diff record length exceeds u64"))?;
        let framed_bytes = record_bytes
            .checked_add(8)
            .ok_or_else(|| corrupt("Diff partition payload length overflowed"))?;
        reserve_temp(framed_bytes)?;
        let length = record_bytes.to_le_bytes();
        self.writer.write_all(&length)?;
        self.writer.write_all(&self.scratch)?;
        self.hasher.update(&length);
        self.hasher.update(&self.scratch);
        self.records = self
            .records
            .checked_add(1)
            .ok_or_else(|| corrupt("Diff partition record count overflowed"))?;
        self.payload_bytes = self
            .payload_bytes
            .checked_add(framed_bytes)
            .ok_or_else(|| corrupt("Diff partition payload length overflowed"))?;
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<(), ProofFrameError> {
        self.writer.flush()?;
        let checksum = *self.hasher.finalize().as_bytes();
        self.writer.seek(SeekFrom::Start(0))?;
        write_header(
            &mut self.writer,
            self.schema_digest,
            self.records,
            self.payload_bytes,
            checksum,
        )?;
        self.writer.flush()?;
        self.writer.get_ref().sync_data()?;
        Ok(())
    }
}

pub(super) struct PartitionReader {
    reader: BufReader<File>,
    remaining_records: u64,
    remaining_payload: u64,
    limits: PartitionLimits,
}

impl PartitionReader {
    pub(super) fn open(
        path: &Path,
        expected_schema: [u8; 32],
        limits: PartitionLimits,
    ) -> Result<Self, ProofFrameError> {
        let header = validate_file(path, expected_schema)?;
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(u64::from(HEADER_BYTES)))?;
        Ok(Self {
            reader: BufReader::new(file),
            remaining_records: header.records,
            remaining_payload: header.payload_bytes,
            limits,
        })
    }

    pub(super) fn next(&mut self) -> Result<Option<(Vec<u8>, RowEntry)>, ProofFrameError> {
        if self.remaining_records == 0 {
            if self.remaining_payload != 0 {
                return Err(corrupt("Diff partition record count leaves payload bytes"));
            }
            let mut trailing = [0_u8; 1];
            if self.reader.read(&mut trailing)? != 0 {
                return Err(corrupt("Diff partition has trailing payload"));
            }
            return Ok(None);
        }
        let record_bytes = read_u64_exact(&mut self.reader)?;
        if record_bytes > self.limits.max_record_bytes {
            return Err(corrupt("Diff partition record exceeds max_record_bytes"));
        }
        let framed_bytes = record_bytes
            .checked_add(8)
            .ok_or_else(|| corrupt("Diff record framing overflowed"))?;
        if framed_bytes > self.remaining_payload {
            return Err(corrupt(
                "Diff record crosses the partition payload boundary",
            ));
        }
        let length = usize::try_from(record_bytes)
            .map_err(|_| corrupt("Diff record length exceeds usize"))?;
        let mut encoded = vec![0_u8; length];
        read_exact_corrupt(&mut self.reader, &mut encoded)?;
        let record = decode_record(&encoded, self.limits)?;
        self.remaining_records -= 1;
        self.remaining_payload -= framed_bytes;
        Ok(Some(record))
    }
}

#[cfg(feature = "fuzzing")]
pub(super) fn fuzz_bytes(input: &[u8]) -> Result<(), ProofFrameError> {
    if input.len() > MAX_FUZZ_INPUT_BYTES {
        return Err(ProofFrameError::ResourceLimit {
            resource: "fuzz partition input",
            requested: input.len() as u64,
            used: 0,
            limit: MAX_FUZZ_INPUT_BYTES as u64,
        });
    }

    let mut file = tempfile::NamedTempFile::new()?;
    file.write_all(input)?;
    file.flush()?;

    let mut expected_schema = [0_u8; 32];
    if let Some(encoded_schema) = input.get(12..44) {
        expected_schema.copy_from_slice(encoded_schema);
    }
    let mut reader = PartitionReader::open(
        file.path(),
        expected_schema,
        PartitionLimits {
            max_record_bytes: MAX_FUZZ_INPUT_BYTES as u64,
            max_columns: 1024,
        },
    )?;
    while reader.next()?.is_some() {}
    Ok(())
}

#[derive(Clone, Copy)]
struct Header {
    schema_digest: [u8; 32],
    records: u64,
    payload_bytes: u64,
    checksum: [u8; 32],
}

fn validate_file(path: &Path, expected_schema: [u8; 32]) -> Result<Header, ProofFrameError> {
    let mut file = File::open(path)?;
    let header = read_header(&mut file)?;
    if header.schema_digest != expected_schema {
        return Err(corrupt("Diff partition schema digest is invalid"));
    }
    let expected_len = u64::from(HEADER_BYTES)
        .checked_add(header.payload_bytes)
        .ok_or_else(|| corrupt("Diff partition file length overflowed"))?;
    if file.metadata()?.len() != expected_len {
        return Err(corrupt("Diff partition file length is invalid"));
    }
    let mut remaining = header.payload_bytes;
    let mut buffer = [0_u8; 8 * 1024];
    let mut hasher = blake3::Hasher::new();
    while remaining != 0 {
        let length = usize::try_from(remaining.min(buffer.len() as u64))
            .expect("bounded checksum length fits usize");
        read_exact_corrupt(&mut file, &mut buffer[..length])?;
        hasher.update(&buffer[..length]);
        remaining -= length as u64;
    }
    if hasher.finalize().as_bytes() != &header.checksum {
        return Err(corrupt("Diff partition payload checksum is invalid"));
    }
    Ok(header)
}

fn write_header(
    writer: &mut impl Write,
    schema_digest: [u8; 32],
    records: u64,
    payload_bytes: u64,
    checksum: [u8; 32],
) -> Result<(), ProofFrameError> {
    writer.write_all(&MAGIC)?;
    writer.write_all(&VERSION.to_le_bytes())?;
    writer.write_all(&HEADER_BYTES.to_le_bytes())?;
    writer.write_all(&schema_digest)?;
    writer.write_all(&records.to_le_bytes())?;
    writer.write_all(&payload_bytes.to_le_bytes())?;
    writer.write_all(&checksum)?;
    Ok(())
}

fn read_header(reader: &mut impl Read) -> Result<Header, ProofFrameError> {
    let mut magic = [0_u8; 8];
    read_exact_corrupt(reader, &mut magic)?;
    if magic != MAGIC {
        return Err(corrupt("Diff partition magic is invalid"));
    }
    let version = read_u16_exact(reader)?;
    if version != VERSION {
        return Err(corrupt("Diff partition version is unsupported"));
    }
    if read_u16_exact(reader)? != HEADER_BYTES {
        return Err(corrupt("Diff partition header length is invalid"));
    }
    let mut schema_digest = [0_u8; 32];
    read_exact_corrupt(reader, &mut schema_digest)?;
    let records = read_u64_exact(reader)?;
    let payload_bytes = read_u64_exact(reader)?;
    let mut checksum = [0_u8; 32];
    read_exact_corrupt(reader, &mut checksum)?;
    Ok(Header {
        schema_digest,
        records,
        payload_bytes,
        checksum,
    })
}

fn decode_record(
    encoded: &[u8],
    limits: PartitionLimits,
) -> Result<(Vec<u8>, RowEntry), ProofFrameError> {
    let mut cursor = Cursor::new(encoded);
    let key = decode_bytes(&mut cursor, limits.max_record_bytes)?;
    let display = decode_bytes(&mut cursor, limits.max_record_bytes)?;
    let display_key =
        String::from_utf8(display).map_err(|_| corrupt("Diff display key is not valid UTF-8"))?;
    let mut hash = [0_u8; 32];
    read_exact_corrupt(&mut cursor, &mut hash)?;
    let value_count = read_u32_exact(&mut cursor)?;
    if value_count > limits.max_columns {
        return Err(corrupt("Diff value count exceeds max_columns"));
    }
    let mut values = Vec::with_capacity(value_count as usize);
    for _ in 0..value_count {
        let length = read_u64_exact(&mut cursor)?;
        if length == u64::MAX {
            values.push(None);
        } else {
            values.push(Some(decode_bytes_with_length(
                &mut cursor,
                length,
                limits.max_record_bytes,
            )?));
        }
    }
    if cursor.position() != encoded.len() as u64 {
        return Err(corrupt("Diff record contains trailing bytes"));
    }
    Ok((
        key,
        RowEntry {
            display_key,
            values,
            hash,
        },
    ))
}

fn encode_bytes(output: &mut Vec<u8>, bytes: &[u8]) -> Result<(), ProofFrameError> {
    let length = u64::try_from(bytes.len()).map_err(|_| corrupt("Diff field exceeds u64"))?;
    output.extend_from_slice(&length.to_le_bytes());
    output.extend_from_slice(bytes);
    Ok(())
}

fn decode_bytes(reader: &mut impl Read, max_bytes: u64) -> Result<Vec<u8>, ProofFrameError> {
    let length = read_u64_exact(reader)?;
    decode_bytes_with_length(reader, length, max_bytes)
}

fn decode_bytes_with_length(
    reader: &mut impl Read,
    length: u64,
    max_bytes: u64,
) -> Result<Vec<u8>, ProofFrameError> {
    if length > max_bytes {
        return Err(corrupt("Diff field length exceeds max_record_bytes"));
    }
    let length = usize::try_from(length).map_err(|_| corrupt("Diff field exceeds usize"))?;
    let mut bytes = vec![0_u8; length];
    read_exact_corrupt(reader, &mut bytes)?;
    Ok(bytes)
}

fn read_u16_exact(reader: &mut impl Read) -> Result<u16, ProofFrameError> {
    let mut bytes = [0_u8; 2];
    read_exact_corrupt(reader, &mut bytes)?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32_exact(reader: &mut impl Read) -> Result<u32, ProofFrameError> {
    let mut bytes = [0_u8; 4];
    read_exact_corrupt(reader, &mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64_exact(reader: &mut impl Read) -> Result<u64, ProofFrameError> {
    let mut bytes = [0_u8; 8];
    read_exact_corrupt(reader, &mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn read_exact_corrupt(reader: &mut impl Read, bytes: &mut [u8]) -> Result<(), ProofFrameError> {
    reader
        .read_exact(bytes)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::UnexpectedEof => corrupt("Diff partition is truncated"),
            _ => ProofFrameError::Io(error),
        })
}

fn corrupt(message: &str) -> ProofFrameError {
    ProofFrameError::CorruptData(message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ErrorCode, ResourceAccount, ResourceLimits};
    use tempfile::TempDir;

    fn valid_partition() -> (TempDir, std::path::PathBuf, [u8; 32]) {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("one.pfpart");
        let schema = [7_u8; 32];
        let mut writer = PartitionWriter::create(&path, schema).unwrap();
        let account = ResourceAccount::root(ResourceLimits::default());
        let mut reservations = Vec::new();
        writer
            .write_record(
                b"key",
                &RowEntry {
                    display_key: "key".to_string(),
                    values: vec![Some(b"value".to_vec())],
                    hash: [9_u8; 32],
                },
                |bytes| {
                    reservations.push(account.try_reserve_temp(bytes)?);
                    Ok(())
                },
            )
            .unwrap();
        writer.finish().unwrap();
        (directory, path, schema)
    }

    #[test]
    fn every_truncated_file_boundary_fails_as_corruption() {
        let (_directory, path, schema) = valid_partition();
        let valid = std::fs::read(&path).unwrap();
        for length in 0..valid.len() {
            let candidate = path.with_extension(format!("truncated-{length}"));
            std::fs::write(&candidate, &valid[..length]).unwrap();
            let error = match PartitionReader::open(
                &candidate,
                schema,
                PartitionLimits {
                    max_record_bytes: 1024,
                    max_columns: 4,
                },
            ) {
                Ok(mut reader) => reader.next().unwrap_err(),
                Err(error) => error,
            };
            assert_eq!(error.code(), ErrorCode::CorruptPartition, "length {length}");
        }
    }

    #[test]
    fn header_and_payload_mutations_fail_closed() {
        let (_directory, path, schema) = valid_partition();
        let valid = std::fs::read(&path).unwrap();
        for offset in [0_usize, 8, 10, 12, 44, 52, 60, 91, 92, valid.len() - 1] {
            let mut mutated = valid.clone();
            mutated[offset] ^= 0x5a;
            let candidate = path.with_extension(format!("mutated-{offset}"));
            std::fs::write(&candidate, mutated).unwrap();
            let opened = PartitionReader::open(
                &candidate,
                schema,
                PartitionLimits {
                    max_record_bytes: 1024,
                    max_columns: 4,
                },
            );
            let error = match opened {
                Err(error) => error,
                Ok(mut reader) => loop {
                    match reader.next() {
                        Err(error) => break error,
                        Ok(Some(_)) => continue,
                        Ok(None) => panic!("mutation at offset {offset} was accepted"),
                    }
                },
            };
            assert_eq!(error.code(), ErrorCode::CorruptPartition, "offset {offset}");
        }
    }

    #[test]
    fn record_limit_is_checked_before_record_allocation() {
        let (_directory, path, schema) = valid_partition();
        let mut reader = PartitionReader::open(
            &path,
            schema,
            PartitionLimits {
                max_record_bytes: 1,
                max_columns: 4,
            },
        )
        .unwrap();
        assert_eq!(
            reader.next().unwrap_err().code(),
            ErrorCode::CorruptPartition
        );
    }
}
