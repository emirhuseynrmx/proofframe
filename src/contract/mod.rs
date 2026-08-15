//! Versioned contract syntax and schema compilation.

mod ast;
mod bounds;
mod compile;

pub use ast::{BoundAst, ContractAst, ContractVersion, NaNPolicyAst, RuleAst};
pub use bounds::TypedBound;
pub use compile::{ColumnPlan, CompiledContract, CompiledRules, KernelKind, NaNPolicy};
