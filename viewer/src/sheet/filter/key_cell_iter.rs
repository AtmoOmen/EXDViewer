use std::rc::Rc;

use compact_str::format_compact;

use crate::{
    excel::provider::ExcelRow,
    sheet::{
        TableContext, cell::CellValue, cell_iter::CellIter, schema_column::SchemaColumn,
        sheet_column::SheetColumnDefinition,
    },
    stopwatch::stopwatches::FILTER_CELL_ITER_STOPWATCH,
};

pub enum KeyCellIter<'a> {
    Columns(CellIter<'a>),
    RowIdOrColumns {
        row_id: u32,
        subrow_id: Option<u16>,
        columns: CellIter<'a>,
        row_id_yielded: bool,
    },
    RowId(u32),
    SubrowId(u32, u16),
    Done,
}

impl<'a> KeyCellIter<'a> {
    pub fn column(
        table: &'a TableContext,
        row: ExcelRow<'a>,
        columns: Rc<Vec<(SchemaColumn, SheetColumnDefinition)>>,
        resolve_display_field: bool,
    ) -> Self {
        Self::Columns(CellIter::new(table, row, columns, resolve_display_field))
    }

    pub fn row_id(row_id: u32, subrow_id: Option<u16>) -> Self {
        if let Some(subrow_id) = subrow_id {
            Self::SubrowId(row_id, subrow_id)
        } else {
            Self::RowId(row_id)
        }
    }

    pub fn row_id_or_column(
        table: &'a TableContext,
        row_id: u32,
        subrow_id: Option<u16>,
        row: ExcelRow<'a>,
        columns: Rc<Vec<(SchemaColumn, SheetColumnDefinition)>>,
        resolve_display_field: bool,
    ) -> Self {
        Self::RowIdOrColumns {
            row_id,
            subrow_id,
            columns: CellIter::new(table, row, columns, resolve_display_field),
            row_id_yielded: false,
        }
    }
}

impl Iterator for KeyCellIter<'_> {
    type Item = anyhow::Result<CellValue>;

    fn next(&mut self) -> Option<Self::Item> {
        let _sw = FILTER_CELL_ITER_STOPWATCH.start();
        match self {
            KeyCellIter::Columns(iter) => iter.next(),
            KeyCellIter::RowIdOrColumns {
                row_id,
                subrow_id,
                columns,
                row_id_yielded,
            } => {
                if !*row_id_yielded {
                    *row_id_yielded = true;
                    Some(Ok(row_id_value(*row_id, *subrow_id)))
                } else {
                    columns.next()
                }
            }
            KeyCellIter::RowId(row_id) => {
                let value = row_id_value(*row_id, None);
                *self = KeyCellIter::Done;
                Some(Ok(value))
            }
            KeyCellIter::SubrowId(row_id, subrow_id) => {
                let value = row_id_value(*row_id, Some(*subrow_id));
                *self = KeyCellIter::Done;
                Some(Ok(value))
            }
            KeyCellIter::Done => None,
        }
    }
}

fn row_id_value(row_id: u32, subrow_id: Option<u16>) -> CellValue {
    if let Some(subrow_id) = subrow_id {
        CellValue::String(format_compact!("{}.{}", row_id, subrow_id).into())
    } else {
        CellValue::Integer(row_id as i128)
    }
}
