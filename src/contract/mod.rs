//! Versioned contract syntax and schema compilation.

mod ast;
mod bounds;
mod compile;
mod dataset;
mod document;
mod row;
mod v2;

pub use ast::{BoundAst, ContractAst, ContractVersion, NaNPolicyAst, RuleAst};
pub use bounds::TypedBound;
pub use compile::{ColumnPlan, CompiledContract, CompiledRules, KernelKind, NaNPolicy};
pub use dataset::{
    BalanceEqualPlan, CompositeUniquePlan, ConditionalUniquePlan, CountPlan, DatasetPlan,
    DominantValuePlan, GapDetectionPlan, MonotonicityPlan, MutuallyExclusivePlan, RatioPlan,
    ReferencePlan, RowCountDeltaPlan, StatisticKind, StatisticPlan, SumBounds, SumPlan,
};
pub use document::ContractDocument;
pub use row::{ComparePlan, OperandPlan, RowPlan, RowPlanKind, ScalarValuePlan};
pub use v2::{
    AssertionAst, BalanceEqualAst, CompareAst, CompareOpAst, CompositeNullPolicyAst,
    CompositeUniqueAst, ConditionalUniqueAst, ContractAstV2, ContractStatus, CountRangeAst,
    DatasetRulesAst, DominantValueAst, ExclusiveModeAst, GapDetectionAst, MonotonicDirectionAst,
    MonotonicNullPolicyAst, MonotonicityAst, MutuallyExclusiveAst, NullPolicyAst, OperandAst,
    ParameterizedTypeAst, PrimitiveTypeAst, RatioRangeAst, ReferenceAst, ReferenceNullPolicyAst,
    RowCountDeltaAst, RowRuleAst, RuleAstV2, StatisticAst, SumAst, TimeUnitAst, TypeAst,
};
