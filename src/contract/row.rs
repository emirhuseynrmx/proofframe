use std::sync::Arc;

use arrow::datatypes::{FieldRef, Schema};

use super::{
    CompareAst, CompareOpAst, CompiledRules, NullPolicyAst, OperandAst, RuleAst,
    compile::compile_rules,
};
use crate::{ErrorCode, KernelKind, ProofFrameError};

#[derive(Debug, Clone)]
pub enum OperandPlan {
    Column {
        column_index: usize,
        field: FieldRef,
        kernel: KernelKind,
    },
    Literal(ScalarValuePlan),
}

#[derive(Debug, Clone)]
pub enum ScalarValuePlan {
    Boolean(bool),
    I64(i64),
    U64(u64),
    F64(f64),
    Text(Box<str>),
}

impl OperandPlan {
    pub(crate) fn data_type(&self) -> Option<&arrow::datatypes::DataType> {
        match self {
            Self::Column { field, .. } => Some(field.data_type()),
            Self::Literal(_) => None,
        }
    }

    pub const fn column_index(&self) -> Option<usize> {
        match self {
            Self::Column { column_index, .. } => Some(*column_index),
            Self::Literal(_) => None,
        }
    }

    #[must_use]
    pub fn field(&self) -> Option<&FieldRef> {
        match self {
            Self::Column { field, .. } => Some(field),
            Self::Literal(_) => None,
        }
    }

    #[must_use]
    pub const fn kernel(&self) -> Option<&KernelKind> {
        match self {
            Self::Column { kernel, .. } => Some(kernel),
            Self::Literal(_) => None,
        }
    }

    #[must_use]
    pub const fn literal(&self) -> Option<&ScalarValuePlan> {
        match self {
            Self::Column { .. } => None,
            Self::Literal(value) => Some(value),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ComparePlan {
    pub(crate) left: OperandPlan,
    pub(crate) op: CompareOpAst,
    pub(crate) right: OperandPlan,
    pub(crate) nulls: NullPolicyAst,
}

impl ComparePlan {
    #[must_use]
    pub const fn left(&self) -> &OperandPlan {
        &self.left
    }

    #[must_use]
    pub const fn op(&self) -> CompareOpAst {
        self.op
    }

    #[must_use]
    pub const fn right(&self) -> &OperandPlan {
        &self.right
    }

    #[must_use]
    pub const fn nulls(&self) -> NullPolicyAst {
        self.nulls
    }
}

#[derive(Debug, Clone)]
pub enum RowPlanKind {
    Compare(ComparePlan),
    Conditional {
        predicate: ComparePlan,
        assertion_column: usize,
        assertion_field: FieldRef,
        assertion: CompiledRules,
    },
}

#[derive(Debug, Clone)]
pub struct RowPlan {
    name: Box<str>,
    pub(crate) kind: RowPlanKind,
}

impl RowPlan {
    pub(crate) fn compile(
        source: &super::RowRuleAst,
        schema: &Schema,
        index: usize,
    ) -> Result<Self, ProofFrameError> {
        let path = format!("$.row_rules[{index}]");
        let kind = if let Some(compare) = source.compare.as_ref() {
            RowPlanKind::Compare(compile_compare(
                compare,
                schema,
                &format!("{path}.compare"),
            )?)
        } else {
            let predicate = source
                .when
                .as_ref()
                .ok_or_else(|| invalid("Conditional row rule is missing `when`", &path))?;
            let assertion = source
                .assertion
                .as_ref()
                .ok_or_else(|| invalid("Conditional row rule is missing `assert`", &path))?;
            let assertion_column = schema.index_of(&assertion.column).map_err(|_| {
                ProofFrameError::contract(
                    ErrorCode::MissingColumn,
                    format!(
                        "Conditional assertion column `{}` is absent",
                        assertion.column
                    ),
                    Some(format!("{path}.assert.column")),
                )
            })?;
            let assertion_field = Arc::new(schema.field(assertion_column).clone());
            let kernel = KernelKind::from_data_type_for_plan(assertion_field.data_type());
            let assertion_source = RuleAst {
                required: false,
                not_null: assertion.not_null,
                unique: false,
                min: assertion.min.clone(),
                max: assertion.max.clone(),
                nan: assertion.nan,
                pattern: assertion.pattern.clone(),
                allowed: assertion.allowed.clone(),
            };
            let compiled_assertion = compile_rules(
                &assertion.column,
                assertion_field.data_type(),
                &kernel,
                &assertion_source,
            )?;
            RowPlanKind::Conditional {
                predicate: compile_compare(predicate, schema, &format!("{path}.when"))?,
                assertion_column,
                assertion_field,
                assertion: compiled_assertion,
            }
        };
        Ok(Self {
            name: source.name.clone().into_boxed_str(),
            kind,
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn kind(&self) -> &RowPlanKind {
        &self.kind
    }
}

fn compile_compare(
    source: &CompareAst,
    schema: &Schema,
    path: &str,
) -> Result<ComparePlan, ProofFrameError> {
    let (left, right) = match (&source.left, &source.right) {
        (OperandAst::Column(left), OperandAst::Column(right)) => (
            compile_column(&left.column, schema, &format!("{path}.left"))?,
            compile_column(&right.column, schema, &format!("{path}.right"))?,
        ),
        (OperandAst::Column(column), OperandAst::Literal(literal)) => {
            let left = compile_column(&column.column, schema, &format!("{path}.left"))?;
            let value = compile_literal(
                &literal.literal,
                left.data_type().expect("column operands have a type"),
                &format!("{path}.right.literal"),
            )?;
            (left, OperandPlan::Literal(value))
        }
        (OperandAst::Literal(literal), OperandAst::Column(column)) => {
            let right = compile_column(&column.column, schema, &format!("{path}.right"))?;
            let value = compile_literal(
                &literal.literal,
                right.data_type().expect("column operands have a type"),
                &format!("{path}.left.literal"),
            )?;
            (OperandPlan::Literal(value), right)
        }
        (OperandAst::Literal(_), OperandAst::Literal(_)) => {
            return Err(invalid(
                "At least one relational operand must name an Arrow column",
                path,
            ));
        }
    };
    if let (Some(left_type), Some(right_type)) = (left.data_type(), right.data_type()) {
        if left_type != right_type {
            return Err(ProofFrameError::contract(
                ErrorCode::ContractTypeMismatch,
                format!(
                    "Relational operands have incompatible types `{left_type}` and `{right_type}`"
                ),
                Some(path.to_string()),
            ));
        }
    }
    Ok(ComparePlan {
        left,
        op: source.op,
        right,
        nulls: source.nulls,
    })
}

fn compile_column(
    column: &str,
    schema: &Schema,
    path: &str,
) -> Result<OperandPlan, ProofFrameError> {
    let index = schema.index_of(column).map_err(|_| {
        ProofFrameError::contract(
            ErrorCode::MissingColumn,
            format!("Relational column `{column}` is absent"),
            Some(format!("{path}.column")),
        )
    })?;
    let field = Arc::new(schema.field(index).clone());
    let kernel = KernelKind::from_data_type_for_plan(field.data_type());
    Ok(OperandPlan::Column {
        column_index: index,
        field,
        kernel,
    })
}

fn compile_literal(
    value: &serde_json::Value,
    data_type: &arrow::datatypes::DataType,
    path: &str,
) -> Result<ScalarValuePlan, ProofFrameError> {
    let parsed = match data_type {
        arrow::datatypes::DataType::Boolean => value.as_bool().map(ScalarValuePlan::Boolean),
        arrow::datatypes::DataType::Int8
        | arrow::datatypes::DataType::Int16
        | arrow::datatypes::DataType::Int32
        | arrow::datatypes::DataType::Int64
        | arrow::datatypes::DataType::Date32
        | arrow::datatypes::DataType::Date64
        | arrow::datatypes::DataType::Timestamp(_, _) => value.as_i64().map(ScalarValuePlan::I64),
        arrow::datatypes::DataType::UInt8
        | arrow::datatypes::DataType::UInt16
        | arrow::datatypes::DataType::UInt32
        | arrow::datatypes::DataType::UInt64 => value.as_u64().map(ScalarValuePlan::U64),
        arrow::datatypes::DataType::Float32 | arrow::datatypes::DataType::Float64 => value
            .as_f64()
            .filter(|value| value.is_finite())
            .map(ScalarValuePlan::F64),
        arrow::datatypes::DataType::Utf8
        | arrow::datatypes::DataType::LargeUtf8
        | arrow::datatypes::DataType::Utf8View => value
            .as_str()
            .map(|value| ScalarValuePlan::Text(value.to_string().into_boxed_str())),
        _ => None,
    };
    parsed.ok_or_else(|| {
        ProofFrameError::contract(
            ErrorCode::ContractTypeMismatch,
            format!("Literal is not representable as Arrow type `{data_type}`"),
            Some(path.to_string()),
        )
    })
}

fn invalid(message: &str, path: &str) -> ProofFrameError {
    ProofFrameError::contract(
        ErrorCode::ContractInvalidJson,
        message,
        Some(path.to_string()),
    )
}
