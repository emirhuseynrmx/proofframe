use arrow::array::{
    Array, BinaryArray, BinaryViewArray, BooleanArray, Date32Array, Date64Array, Decimal128Array,
    FixedSizeListArray, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Int64Array,
    LargeBinaryArray, LargeListArray, LargeStringArray, ListArray, MapArray, StringArray,
    StringViewArray, StructArray, TimestampMicrosecondArray, TimestampMillisecondArray,
    TimestampNanosecondArray, TimestampSecondArray, UInt8Array, UInt16Array, UInt32Array,
    UInt64Array,
};
use arrow::datatypes::{DataType, Schema};

use crate::ProofFrameError;

/// Schema-specialized canonical value encoder.
pub(crate) enum ValueEncoder {
    Int8,
    Int16,
    Int32,
    Int64,
    UInt8,
    UInt16,
    UInt32,
    UInt64,
    Float32,
    Float64,
    Date32,
    Date64,
    TimestampSecond,
    TimestampMillisecond,
    TimestampMicrosecond,
    TimestampNanosecond,
    Decimal128,
    Boolean,
    Utf8,
    LargeUtf8,
    Utf8View,
    Binary,
    LargeBinary,
    BinaryView,
    List(Box<Self>),
    LargeList(Box<Self>),
    FixedSizeList(Box<Self>),
    Struct(Vec<Self>),
    Map(Box<Self>),
}

impl ValueEncoder {
    pub(crate) fn for_data_type(data_type: &DataType) -> Result<Self, ProofFrameError> {
        if let Some(encoder) = Self::scalar_encoder(data_type) {
            return Ok(encoder);
        }
        if let Some(encoder) = Self::temporal_encoder(data_type) {
            return Ok(encoder);
        }
        if let Some(encoder) = Self::nested_encoder(data_type)? {
            return Ok(encoder);
        }
        Err(ProofFrameError::UnsupportedType(data_type.to_string()))
    }

    fn scalar_encoder(data_type: &DataType) -> Option<Self> {
        match data_type {
            DataType::Int8 => Some(Self::Int8),
            DataType::Int16 => Some(Self::Int16),
            DataType::Int32 => Some(Self::Int32),
            DataType::Int64 => Some(Self::Int64),
            DataType::UInt8 => Some(Self::UInt8),
            DataType::UInt16 => Some(Self::UInt16),
            DataType::UInt32 => Some(Self::UInt32),
            DataType::UInt64 => Some(Self::UInt64),
            DataType::Float32 => Some(Self::Float32),
            DataType::Float64 => Some(Self::Float64),
            DataType::Decimal128(_, _) => Some(Self::Decimal128),
            DataType::Boolean => Some(Self::Boolean),
            DataType::Utf8 => Some(Self::Utf8),
            DataType::LargeUtf8 => Some(Self::LargeUtf8),
            DataType::Utf8View => Some(Self::Utf8View),
            DataType::Binary => Some(Self::Binary),
            DataType::LargeBinary => Some(Self::LargeBinary),
            DataType::BinaryView => Some(Self::BinaryView),
            _ => None,
        }
    }

    fn temporal_encoder(data_type: &DataType) -> Option<Self> {
        match data_type {
            DataType::Date32 => Some(Self::Date32),
            DataType::Date64 => Some(Self::Date64),
            DataType::Timestamp(arrow::datatypes::TimeUnit::Second, _) => {
                Some(Self::TimestampSecond)
            }
            DataType::Timestamp(arrow::datatypes::TimeUnit::Millisecond, _) => {
                Some(Self::TimestampMillisecond)
            }
            DataType::Timestamp(arrow::datatypes::TimeUnit::Microsecond, _) => {
                Some(Self::TimestampMicrosecond)
            }
            DataType::Timestamp(arrow::datatypes::TimeUnit::Nanosecond, _) => {
                Some(Self::TimestampNanosecond)
            }
            _ => None,
        }
    }

    fn nested_encoder(data_type: &DataType) -> Result<Option<Self>, ProofFrameError> {
        let encoder = match data_type {
            DataType::List(field) => Self::List(Box::new(Self::for_data_type(field.data_type())?)),
            DataType::LargeList(field) => {
                Self::LargeList(Box::new(Self::for_data_type(field.data_type())?))
            }
            DataType::FixedSizeList(field, _) => {
                Self::FixedSizeList(Box::new(Self::for_data_type(field.data_type())?))
            }
            DataType::Struct(fields) => Self::Struct(
                fields
                    .iter()
                    .map(|field| Self::for_data_type(field.data_type()))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            DataType::Map(field, _) => Self::Map(Box::new(Self::for_data_type(field.data_type())?)),
            _ => return Ok(None),
        };
        Ok(Some(encoder))
    }

    pub(crate) fn update_v1(
        &self,
        hasher: &mut blake3::Hasher,
        column: usize,
        array: &dyn Array,
        row: usize,
        scratch: &mut Vec<u8>,
    ) -> Result<(), ProofFrameError> {
        hasher.update(b"pf-cell-v1\0");
        hasher.update(&(column as u64).to_le_bytes());
        if array.is_null(row) {
            hasher.update(&1_u64.to_le_bytes());
            hasher.update(&[0]);
            return Ok(());
        }

        macro_rules! direct_primitive {
            ($variant:ident, $array_ty:ty, $tag:literal) => {
                if matches!(self, Self::$variant) {
                    let values = array
                        .as_any()
                        .downcast_ref::<$array_ty>()
                        .expect("encoder is fixed from the Arrow schema");
                    let bytes = values.value(row).to_le_bytes();
                    hasher.update(&(1_u64 + bytes.len() as u64).to_le_bytes());
                    hasher.update(&[$tag]);
                    hasher.update(&bytes);
                    return Ok(());
                }
            };
        }

        direct_primitive!(Int8, Int8Array, 1);
        direct_primitive!(Int16, Int16Array, 2);
        direct_primitive!(Int32, Int32Array, 3);
        direct_primitive!(Int64, Int64Array, 4);
        direct_primitive!(UInt8, UInt8Array, 5);
        direct_primitive!(UInt16, UInt16Array, 6);
        direct_primitive!(UInt32, UInt32Array, 7);
        direct_primitive!(UInt64, UInt64Array, 8);
        direct_primitive!(Float32, Float32Array, 9);
        direct_primitive!(Float64, Float64Array, 10);
        direct_primitive!(Date32, Date32Array, 11);
        direct_primitive!(Date64, Date64Array, 12);
        direct_primitive!(TimestampSecond, TimestampSecondArray, 13);
        direct_primitive!(TimestampMillisecond, TimestampMillisecondArray, 14);
        direct_primitive!(TimestampMicrosecond, TimestampMicrosecondArray, 15);
        direct_primitive!(TimestampNanosecond, TimestampNanosecondArray, 16);
        direct_primitive!(Decimal128, Decimal128Array, 17);

        if matches!(self, Self::Boolean) {
            let values = array
                .as_any()
                .downcast_ref::<BooleanArray>()
                .expect("encoder is fixed from the Arrow schema");
            hasher.update(&2_u64.to_le_bytes());
            hasher.update(&[18, u8::from(values.value(row))]);
            return Ok(());
        }

        macro_rules! direct_string {
            ($variant:ident, $array_ty:ty, $tag:literal) => {
                if matches!(self, Self::$variant) {
                    let values = array
                        .as_any()
                        .downcast_ref::<$array_ty>()
                        .expect("encoder is fixed from the Arrow schema");
                    let value = values.value(row).as_bytes();
                    hasher.update(&(9_u64 + value.len() as u64).to_le_bytes());
                    hasher.update(&[$tag]);
                    hasher.update(&(value.len() as u64).to_le_bytes());
                    hasher.update(value);
                    return Ok(());
                }
            };
        }

        macro_rules! direct_binary {
            ($variant:ident, $array_ty:ty, $tag:literal) => {
                if matches!(self, Self::$variant) {
                    let values = array
                        .as_any()
                        .downcast_ref::<$array_ty>()
                        .expect("encoder is fixed from the Arrow schema");
                    let value = values.value(row);
                    hasher.update(&(9_u64 + value.len() as u64).to_le_bytes());
                    hasher.update(&[$tag]);
                    hasher.update(&(value.len() as u64).to_le_bytes());
                    hasher.update(value);
                    return Ok(());
                }
            };
        }

        direct_string!(Utf8, StringArray, 19);
        direct_string!(LargeUtf8, LargeStringArray, 20);
        direct_string!(Utf8View, StringViewArray, 28);
        direct_binary!(Binary, BinaryArray, 21);
        direct_binary!(LargeBinary, LargeBinaryArray, 22);
        direct_binary!(BinaryView, BinaryViewArray, 29);

        scratch.clear();
        self.write_v1_value(array, row, scratch)?;
        hasher.update(&(scratch.len() as u64).to_le_bytes());
        hasher.update(scratch);
        Ok(())
    }

    pub(crate) fn update_v2(
        &self,
        hasher: &mut blake3::Hasher,
        array: &dyn Array,
        row: usize,
        scratch: &mut Vec<u8>,
    ) -> Result<(), ProofFrameError> {
        if array.is_null(row) {
            hasher.update(&[0]);
            return Ok(());
        }
        hasher.update(&[1]);

        macro_rules! direct_primitive {
            ($variant:ident, $array_ty:ty) => {
                if matches!(self, Self::$variant) {
                    let values = array
                        .as_any()
                        .downcast_ref::<$array_ty>()
                        .expect("encoder is fixed from the Arrow schema");
                    hasher.update(&values.value(row).to_le_bytes());
                    return Ok(());
                }
            };
        }

        direct_primitive!(Int8, Int8Array);
        direct_primitive!(Int16, Int16Array);
        direct_primitive!(Int32, Int32Array);
        direct_primitive!(Int64, Int64Array);
        direct_primitive!(UInt8, UInt8Array);
        direct_primitive!(UInt16, UInt16Array);
        direct_primitive!(UInt32, UInt32Array);
        direct_primitive!(UInt64, UInt64Array);
        direct_primitive!(Float32, Float32Array);
        direct_primitive!(Float64, Float64Array);
        direct_primitive!(Date32, Date32Array);
        direct_primitive!(Date64, Date64Array);
        direct_primitive!(TimestampSecond, TimestampSecondArray);
        direct_primitive!(TimestampMillisecond, TimestampMillisecondArray);
        direct_primitive!(TimestampMicrosecond, TimestampMicrosecondArray);
        direct_primitive!(TimestampNanosecond, TimestampNanosecondArray);
        direct_primitive!(Decimal128, Decimal128Array);

        if matches!(self, Self::Boolean) {
            let values = downcast::<BooleanArray>(array);
            hasher.update(&[u8::from(values.value(row))]);
            return Ok(());
        }

        macro_rules! direct_string {
            ($variant:ident, $array_ty:ty) => {
                if matches!(self, Self::$variant) {
                    let value = downcast::<$array_ty>(array).value(row).as_bytes();
                    hasher.update(&(value.len() as u64).to_le_bytes());
                    hasher.update(value);
                    return Ok(());
                }
            };
        }
        macro_rules! direct_binary {
            ($variant:ident, $array_ty:ty) => {
                if matches!(self, Self::$variant) {
                    let value = downcast::<$array_ty>(array).value(row);
                    hasher.update(&(value.len() as u64).to_le_bytes());
                    hasher.update(value);
                    return Ok(());
                }
            };
        }

        direct_string!(Utf8, StringArray);
        direct_string!(LargeUtf8, LargeStringArray);
        direct_string!(Utf8View, StringViewArray);
        direct_binary!(Binary, BinaryArray);
        direct_binary!(LargeBinary, LargeBinaryArray);
        direct_binary!(BinaryView, BinaryViewArray);

        scratch.clear();
        self.write_v1_value(array, row, scratch)?;
        hasher.update(&(scratch.len() as u64).to_le_bytes());
        hasher.update(scratch);
        Ok(())
    }

    fn write_v1_value(
        &self,
        array: &dyn Array,
        row: usize,
        output: &mut Vec<u8>,
    ) -> Result<(), ProofFrameError> {
        if array.is_null(row) {
            output.push(0);
            return Ok(());
        }

        macro_rules! primitive {
            ($variant:ident, $array_ty:ty, $tag:literal) => {
                if matches!(self, Self::$variant) {
                    let values = array
                        .as_any()
                        .downcast_ref::<$array_ty>()
                        .expect("encoder is fixed from the Arrow schema");
                    output.push($tag);
                    output.extend_from_slice(&values.value(row).to_le_bytes());
                    return Ok(());
                }
            };
        }

        primitive!(Int8, Int8Array, 1);
        primitive!(Int16, Int16Array, 2);
        primitive!(Int32, Int32Array, 3);
        primitive!(Int64, Int64Array, 4);
        primitive!(UInt8, UInt8Array, 5);
        primitive!(UInt16, UInt16Array, 6);
        primitive!(UInt32, UInt32Array, 7);
        primitive!(UInt64, UInt64Array, 8);
        primitive!(Float32, Float32Array, 9);
        primitive!(Float64, Float64Array, 10);
        primitive!(Date32, Date32Array, 11);
        primitive!(Date64, Date64Array, 12);
        primitive!(TimestampSecond, TimestampSecondArray, 13);
        primitive!(TimestampMillisecond, TimestampMillisecondArray, 14);
        primitive!(TimestampMicrosecond, TimestampMicrosecondArray, 15);
        primitive!(TimestampNanosecond, TimestampNanosecondArray, 16);
        primitive!(Decimal128, Decimal128Array, 17);

        match self {
            Self::Boolean => {
                let values = downcast::<BooleanArray>(array);
                output.extend_from_slice(&[18, u8::from(values.value(row))]);
            }
            Self::Utf8 => write_bytes_value(
                output,
                19,
                downcast::<StringArray>(array).value(row).as_bytes(),
            ),
            Self::LargeUtf8 => write_bytes_value(
                output,
                20,
                downcast::<LargeStringArray>(array).value(row).as_bytes(),
            ),
            Self::Utf8View => write_bytes_value(
                output,
                28,
                downcast::<StringViewArray>(array).value(row).as_bytes(),
            ),
            Self::Binary => {
                write_bytes_value(output, 21, downcast::<BinaryArray>(array).value(row))
            }
            Self::LargeBinary => {
                write_bytes_value(output, 22, downcast::<LargeBinaryArray>(array).value(row))
            }
            Self::BinaryView => {
                write_bytes_value(output, 29, downcast::<BinaryViewArray>(array).value(row))
            }
            Self::List(child_encoder) => {
                let child = downcast::<ListArray>(array).value(row);
                write_child_sequence(output, 23, child.as_ref(), child_encoder)?;
            }
            Self::LargeList(child_encoder) => {
                let child = downcast::<LargeListArray>(array).value(row);
                write_child_sequence(output, 24, child.as_ref(), child_encoder)?;
            }
            Self::FixedSizeList(child_encoder) => {
                let child = downcast::<FixedSizeListArray>(array).value(row);
                write_child_sequence(output, 25, child.as_ref(), child_encoder)?;
            }
            Self::Struct(children) => {
                let values = downcast::<StructArray>(array);
                output.push(26);
                output.extend_from_slice(&(values.num_columns() as u64).to_le_bytes());
                for (column, encoder) in values.columns().iter().zip(children) {
                    write_len_prefixed(output, |output| {
                        encoder.write_v1_value(column.as_ref(), row, output)
                    })?;
                }
            }
            Self::Map(child_encoder) => {
                let child = downcast::<MapArray>(array).value(row);
                write_child_sequence(output, 27, &child, child_encoder)?;
            }
            _ => unreachable!("primitive encoder returned before nested dispatch"),
        }
        Ok(())
    }
}

pub(crate) struct EncodingPlan {
    pub(crate) encoders: Vec<ValueEncoder>,
}

impl EncodingPlan {
    pub(crate) fn for_schema(schema: &Schema) -> Result<Self, ProofFrameError> {
        let encoders = schema
            .fields()
            .iter()
            .map(|field| ValueEncoder::for_data_type(field.data_type()))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { encoders })
    }
}

fn downcast<T: 'static>(array: &dyn Array) -> &T {
    array
        .as_any()
        .downcast_ref::<T>()
        .expect("encoder is fixed from the Arrow schema")
}

fn write_bytes_value(output: &mut Vec<u8>, tag: u8, value: &[u8]) {
    output.push(tag);
    output.extend_from_slice(&(value.len() as u64).to_le_bytes());
    output.extend_from_slice(value);
}

fn write_child_sequence(
    output: &mut Vec<u8>,
    tag: u8,
    child: &dyn Array,
    encoder: &ValueEncoder,
) -> Result<(), ProofFrameError> {
    output.push(tag);
    output.extend_from_slice(&(child.len() as u64).to_le_bytes());
    for index in 0..child.len() {
        write_len_prefixed(output, |output| {
            encoder.write_v1_value(child, index, output)
        })?;
    }
    Ok(())
}

fn write_len_prefixed(
    output: &mut Vec<u8>,
    write: impl FnOnce(&mut Vec<u8>) -> Result<(), ProofFrameError>,
) -> Result<(), ProofFrameError> {
    let length_offset = output.len();
    output.extend_from_slice(&0_u64.to_le_bytes());
    let value_offset = output.len();
    write(output)?;
    let length = (output.len() - value_offset) as u64;
    output[length_offset..value_offset].copy_from_slice(&length.to_le_bytes());
    Ok(())
}
