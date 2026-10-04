use std::rc::Rc;

use crate::sheet::{
    filter::complex_filter::FilterValue, schema_column::SchemaColumn,
    sheet_column::SheetColumnDefinition,
};

#[derive(Debug, Clone)]
pub struct CompiledComplexFilter {
    pub filter: CompiledFilterPart,
    pub lookup: Vec<CompiledFilterKey>,
    pub has_fuzzy: bool,
}

#[derive(Debug, Clone)]
pub enum CompiledFilterKey {
    RowId,
    RowIdOrColumn(Rc<Vec<(SchemaColumn, SheetColumnDefinition)>>),
    Column(Rc<Vec<(SchemaColumn, SheetColumnDefinition)>>, bool),
}

impl CompiledFilterKey {
    pub fn is_strict(&self) -> bool {
        match self {
            CompiledFilterKey::RowId => true,
            CompiledFilterKey::RowIdOrColumn(_) => false,
            CompiledFilterKey::Column(_, is_strict) => *is_strict,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CompiledFilterPart {
    /// A simple key-value filter
    /// (u32 is the lookup index in `CompiledComplexFilter.lookup`)
    KeyEquals(u32, FilterValue),
    /// Combine two filters with logical AND
    And(Vec<CompiledFilterPart>),
    // Combine two filters with logical OR
    Or(Vec<CompiledFilterPart>),
    /// Negate a filter with logical NOT
    Not(Box<CompiledFilterPart>),
}

impl CompiledFilterPart {
    /// 展开条件中每一处 `KeyEquals`, 给出其 lookup 索引与比较值。用于在行已经命中之后
    /// 逐列回查是哪几列把条件满足了, 因此不看逻辑连接符本身。
    pub fn key_equals(&self, into: &mut Vec<(u32, FilterValue)>) {
        match self {
            Self::KeyEquals(key, value) => into.push((*key, value.clone())),
            Self::And(parts) | Self::Or(parts) => {
                parts.iter().for_each(|part| part.key_equals(into));
            }
            Self::Not(part) => part.key_equals(into),
        }
    }
}
