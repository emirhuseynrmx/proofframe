use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::{ArrayRef, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::ipc::writer::FileWriter;
use arrow::record_batch::RecordBatch;
use serde::Serialize;
use tempfile::NamedTempFile;

use super::DiffOutput;
use crate::{ProofFrameError, ResourceAccount};

#[derive(Serialize)]
pub(super) struct DiffEvent<'a> {
    pub kind: &'static str,
    pub key: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub columns: Option<&'a [String]>,
}

pub(super) struct AtomicSink {
    target: PathBuf,
    temporary: Option<NamedTempFile>,
    writer: Option<SinkWriter>,
    scratch: Vec<u8>,
    _memory: crate::MemoryReservation,
    account: ResourceAccount,
    scratch_memory: Vec<crate::MemoryReservation>,
}

impl AtomicSink {
    pub(super) fn new(
        output: &DiffOutput,
        account: &ResourceAccount,
    ) -> Result<Self, ProofFrameError> {
        let target = match output {
            DiffOutput::JsonLines(path) | DiffOutput::ArrowIpc(path) => path.clone(),
        };
        let parent = target
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if !parent.is_dir() {
            return Err(ProofFrameError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Diff output parent directory does not exist",
            )));
        }
        let temporary = NamedTempFile::new_in(parent)?;
        let writer = match output {
            DiffOutput::JsonLines(_) => {
                SinkWriter::Json(BufWriter::with_capacity(8 * 1024, temporary.reopen()?))
            }
            DiffOutput::ArrowIpc(_) => {
                SinkWriter::Arrow(FileWriter::try_new(temporary.reopen()?, &event_schema())?)
            }
        };
        Ok(Self {
            target,
            temporary: Some(temporary),
            writer: Some(writer),
            scratch: Vec::with_capacity(1024),
            _memory: account.try_reserve_memory(8 * 1024 + 1024)?,
            account: account.clone(),
            scratch_memory: Vec::new(),
        })
    }

    pub(super) fn write(
        &mut self,
        event: &DiffEvent<'_>,
        reserve_temp: impl FnOnce(u64) -> Result<(), ProofFrameError>,
    ) -> Result<(), ProofFrameError> {
        if matches!(self.writer.as_ref(), Some(SinkWriter::Arrow(_))) {
            let columns = event.columns.map(serde_json::to_string).transpose()?;
            let estimated = event
                .key
                .len()
                .checked_add(columns.as_ref().map_or(0, String::len))
                .and_then(|bytes| bytes.checked_mul(4))
                .and_then(|bytes| bytes.checked_add(4096))
                .and_then(|bytes| u64::try_from(bytes).ok())
                .ok_or_else(|| {
                    ProofFrameError::CorruptData("Arrow diff event is too large".into())
                })?;
            reserve_temp(estimated)?;
            let _event_memory = self.account.try_reserve_memory(estimated)?;
            let batch = RecordBatch::try_new(
                event_schema(),
                vec![
                    Arc::new(StringArray::from(vec![event.kind])) as ArrayRef,
                    Arc::new(StringArray::from(vec![event.key])) as ArrayRef,
                    Arc::new(StringArray::from(vec![columns.as_deref()])) as ArrayRef,
                ],
            )?;
            if let Some(SinkWriter::Arrow(writer)) = self.writer.as_mut() {
                writer.write(&batch)?;
            }
            return Ok(());
        }
        let mut counter = CountingWriter::default();
        serde_json::to_writer(&mut counter, event)?;
        let required = counter
            .bytes
            .checked_add(1)
            .and_then(|bytes| usize::try_from(bytes).ok())
            .ok_or_else(|| {
                ProofFrameError::CorruptData("Diff output record is too large".into())
            })?;
        self.scratch.clear();
        if required > self.scratch.capacity() {
            let growth = required - self.scratch.capacity();
            self.scratch_memory
                .push(self.account.try_reserve_memory(growth as u64)?);
            self.scratch.reserve_exact(required);
        }
        serde_json::to_writer(&mut self.scratch, event)?;
        self.scratch.push(b'\n');
        reserve_temp(self.scratch.len() as u64)?;
        if let Some(SinkWriter::Json(writer)) = self.writer.as_mut() {
            writer.write_all(&self.scratch)?;
        }
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<(), ProofFrameError> {
        match self.writer.take().expect("sink is unfinished") {
            SinkWriter::Json(mut writer) => {
                writer.flush()?;
                writer.get_ref().sync_data()?;
            }
            SinkWriter::Arrow(mut writer) => {
                writer.finish()?;
                writer.get_ref().sync_data()?;
            }
        }
        let temporary = self.temporary.take().expect("sink is unfinished");
        temporary
            .persist(&self.target)
            .map_err(|error| ProofFrameError::Io(error.error))?;
        Ok(())
    }
}

enum SinkWriter {
    Json(BufWriter<File>),
    Arrow(FileWriter<File>),
}

fn event_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("kind", DataType::Utf8, false),
        Field::new("key", DataType::Utf8, false),
        Field::new("columns_json", DataType::Utf8, true),
    ]))
}

#[derive(Default)]
struct CountingWriter {
    bytes: u64,
}

impl Write for CountingWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(buffer.len() as u64)
            .ok_or_else(|| std::io::Error::other("serialized diff event exceeds u64"))?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
