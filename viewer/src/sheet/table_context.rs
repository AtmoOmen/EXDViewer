use std::{
    borrow::Cow,
    cell::{Cell as ValueCell, RefCell},
    collections::HashMap,
    num::NonZeroU32,
    rc::Rc,
};

use anyhow::bail;
use ironworks::file::exh::ColumnKind;
use itertools::Itertools;

use crate::{
    excel::{
        base::BaseSheet,
        provider::{ExcelHeader, ExcelProvider, ExcelRow, ExcelSheet},
    },
    schema::{Schema, provider::SchemaProvider},
    sheet::{
        cell::MatchOptions,
        filter::{CompiledFilterInput, CompiledFilterKey, FilterCache, FilterInput, KeyCellIter},
    },
    stopwatch::stopwatches::{FILTER_CELL_GRAB_STOPWATCH, FILTER_ROW_STOPWATCH},
    utils::{CloneableResult, ConvertiblePromise, TrackedPromise},
};

use super::{
cell::{Cell, CellValue, read_integer},
    global_context::GlobalContext,
    schema_column::{ResolvedTableContext, SchemaColumn, SchemaColumnMeta},
    sheet_column::SheetColumnDefinition,
};

type SheetPromise = TrackedPromise<anyhow::Result<(BaseSheet, Option<Schema>)>>;
type ConvertibleSheetPromise = ConvertiblePromise<SheetPromise, CloneableResult<TableContext>>;
pub type SharedConvertibleSheetPromise = Rc<RefCell<ConvertibleSheetPromise>>;

#[derive(Clone)]
pub struct TableContext(Rc<TableContextImpl>);

pub struct TableContextImpl {
    global: GlobalContext,

    sheet: BaseSheet,

    // ID -> Index when ordered by offset (offset index)
    column_ordering: Vec<u32>,
    sheet_columns: Vec<SheetColumnDefinition>,
    schema_columns: RefCell<Vec<SchemaColumn>>,
    sizing_columns: RefCell<Vec<u32>>,
    has_icon_column: ValueCell<bool>,
    // Offset index of the displayField column
    display_column_idx: ValueCell<Option<u32>>,

    referenced_sheets: RefCell<HashMap<String, SharedConvertibleSheetPromise>>,

    filter_cache: FilterCache,
}

impl TableContext {
    pub fn new(global: GlobalContext, sheet: BaseSheet, schema: Option<&Schema>) -> Self {
        let sheet_columns = SheetColumnDefinition::from_sheet(&sheet);
        let (schema_columns, display_column_idx) = schema
            .and_then(|s| SchemaColumn::from_schema(s).ok())
            .unwrap_or_else(|| (SchemaColumn::from_blank(sheet_columns.len()), None));
        let column_ordering = sheet_columns
            .iter()
            .enumerate()
            .sorted_by_key(|&(_i, p)| p.id)
            .map(|(i, _p)| i as u32)
            .collect_vec();

        let filter_cache = FilterCache::new(&schema_columns, &sheet_columns);

        let context = Self(Rc::new(TableContextImpl {
            global,
            sheet,
            column_ordering,
            sheet_columns,
            schema_columns: RefCell::new(schema_columns),
            sizing_columns: RefCell::new(Vec::new()),
            has_icon_column: ValueCell::new(false),
            display_column_idx: ValueCell::new(display_column_idx),
            referenced_sheets: RefCell::new(HashMap::new()),
            filter_cache,
        }));
        context.update_sizing_columns();
        context
    }

    pub fn sheet(&self) -> &BaseSheet {
        &self.0.sheet
    }

    pub fn global(&self) -> &GlobalContext {
        &self.0.global
    }

    pub fn get_column_by_offset(
        &self,
        column_idx: u32,
    ) -> anyhow::Result<(SchemaColumn, &SheetColumnDefinition)> {
        Ok((
            self.0
                .schema_columns
                .borrow()
                .get(column_idx as usize)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Column index out of bounds: {} >= {}",
                        column_idx,
                        self.0.sheet_columns.len()
                    )
                })?
                .clone(),
            self.0
                .sheet_columns
                .get(column_idx as usize)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Column index out of bounds: {} >= {}",
                        column_idx,
                        self.0.sheet_columns.len()
                    )
                })?,
        ))
    }

    pub fn convert_column_index_to_offset_index(&self, column_idx: u32) -> anyhow::Result<u32> {
        self.0
            .column_ordering
            .get(column_idx as usize)
            .copied()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Column index out of bounds: {} >= {}",
                    column_idx,
                    self.0.column_ordering.len()
                )
            })
    }

    pub fn get_column_by_index(
        &self,
        column_idx: u32,
    ) -> anyhow::Result<((SchemaColumn, &SheetColumnDefinition), u32)> {
        let offset_idx = self.convert_column_index_to_offset_index(column_idx)?;
        Ok((self.get_column_by_offset(offset_idx)?, offset_idx))
    }

    pub fn set_schema(&self, schema: Option<&Schema>) -> anyhow::Result<()> {
        let schema = schema.map_or_else(
            || {
                SchemaColumn::from_schema(&Schema::from_blank(
                    self.0.sheet.name(),
                    self.0.sheet_columns.len(),
                ))
            },
            SchemaColumn::from_schema,
        );
        let (columns, display_column_idx) = schema.and_then(|r| {
            if r.0.len() != self.0.sheet_columns.len() {
                bail!(
                    "Schema column count does not match sheet column count: {} != {}",
                    r.0.len(),
                    self.0.sheet_columns.len()
                )
            }
            Ok(r)
        })?;
        self.0.schema_columns.replace(columns);
        self.0.display_column_idx.replace(display_column_idx);
        self.update_sizing_columns();
        self.0.filter_cache.invalidate_cache(self)?;
        Ok(())
    }

    pub fn load_sheets(&self, names: &[String]) -> Vec<SharedConvertibleSheetPromise> {
        let mut sheets = self.0.referenced_sheets.borrow_mut();
        names
            .iter()
            .map(|name| {
                sheets
                    .entry(name.clone())
                    .or_insert_with_key(|name| {
                        let ctx = self.0.global.clone();
                        let name = name.clone();
                        let promise = ConvertiblePromise::new_promise(TrackedPromise::spawn_local(
                            async move {
                                let sheet_future =
                                    ctx.backend().excel().get_sheet(&name, ctx.language());
                                let schema_future = ctx.backend().schema().get_schema_text(&name);
                                Ok(futures_util::try_join!(sheet_future, async move {
                                    Ok(schema_future
                                        .await
                                        .and_then(|s| Schema::from_str(&s))
                                        .map(|a| a.ok())
                                        .ok()
                                        .flatten())
                                })?)
                            },
                        ));
                        Rc::new(RefCell::new(promise))
                    })
                    .clone()
            })
            .collect()
    }

    pub fn has_pending_references(&self) -> bool {
        self.0.referenced_sheets.borrow().values().any(|promise| {
            let promise = promise.borrow();
            !promise.converted() && !promise.should_swap()
        })
    }

    pub fn columns(&self) -> anyhow::Result<Vec<(SchemaColumn, SheetColumnDefinition)>> {
        (0..self.0.sheet_columns.len() as u32)
            .map(|i| self.get_column_by_offset(i))
            .map_ok(|(schema_col, sheet_col)| (schema_col, sheet_col.clone()))
            .collect::<anyhow::Result<Vec<_>>>()
    }

    pub fn column_count(&self) -> usize {
        self.0.sheet_columns.len()
    }

    pub fn cell_by_offset<'a>(
        &'a self,
        row: ExcelRow<'a>,
        column_idx: u32,
    ) -> anyhow::Result<Cell<'a>> {
        let (schema_column, sheet_column) = self.get_column_by_offset(column_idx)?;
        Ok(Cell::new(
            row,
            Cow::Owned(schema_column),
            sheet_column,
            self,
        ))
    }

    pub fn cell_by_index<'a>(
        &'a self,
        row: ExcelRow<'a>,
        column_idx: u32,
    ) -> anyhow::Result<Cell<'a>> {
        let ((schema_column, sheet_column), _offset_idx) = self.get_column_by_index(column_idx)?;
        Ok(Cell::new(
            row,
            Cow::Owned(schema_column),
            sheet_column,
            self,
        ))
    }

    /// The icon a `Link` column's target row itself names under `field`, once the target sheet has
    /// loaded and the row is found. Used where a linked row's own icon belongs beside its resolved
    /// name, which the display field alone does not carry.
    pub fn linked_icon(&self, at: u32, row: ExcelRow<'_>, field: &str) -> Option<u32> {
        let (schema_column, sheet_column) = self.get_column_by_offset(at).ok()?;
        let SchemaColumnMeta::Link(sheets) = schema_column.meta() else {
            return None;
        };
        let row_id: i128 =
            read_integer(row, sheet_column.offset() as u32, sheet_column.kind()).ok()?;
        let row_id = u32::try_from(row_id).ok().filter(|id| *id != 0)?;
        let ResolvedTableContext::Found { table: linked, .. } = sheets.resolve(self, row_id) else {
            return None;
        };
        let target_row = linked.sheet().get_row(row_id).ok()?;
        let columns = linked.columns().ok()?;
        let idx = columns.iter().position(|(c, _)| c.name() == field)?;
        let cell = linked.cell_by_offset(target_row, idx as u32).ok()?;
        match cell.read(false).ok()? {
            CellValue::Icon(icon_id) => u32::try_from(icon_id).ok().filter(|id| *id != 0),
            _ => None,
        }
    }

    pub fn display_column_idx(&self) -> Option<u32> {
        self.0.display_column_idx.get()
    }

    pub fn display_field_cell<'a>(&'a self, row: ExcelRow<'a>) -> Option<anyhow::Result<Cell<'a>>> {
        Some(self.cell_by_offset(row, self.0.display_column_idx.get()?))
    }

    pub fn size_row(
        &self,
        row: ExcelRow<'_>,
        ui: &mut egui::Ui,
        row_location: (u32, Option<u16>),
    ) -> f32 {
        let size = self
            .0
            .sizing_columns
            .borrow()
            .iter()
            .filter_map(|column_idx| self.cell_by_offset(row, *column_idx).ok())
            .map(|c| c.size(ui, row_location))
            .reduce(f32::max);
        let minimum = if self.0.has_icon_column.get() {
            32.0
        } else {
            ui.text_style_height(&egui::TextStyle::Body)
        };
        size.unwrap_or(minimum).max(minimum) + 4.0
    }

    fn update_sizing_columns(&self) {
        let schema_columns = self.0.schema_columns.borrow();
        let mut sizing_columns = self.0.sizing_columns.borrow_mut();
        sizing_columns.clear();
        self.0.has_icon_column.set(false);

        for (index, (schema_column, sheet_column)) in
            schema_columns.iter().zip(&self.0.sheet_columns).enumerate()
        {
            match schema_column.meta() {
                SchemaColumnMeta::Scalar if sheet_column.kind() == ColumnKind::String => {
                    sizing_columns.push(index as u32);
                }
                SchemaColumnMeta::Icon => self.0.has_icon_column.set(true),
                _ => {}
            }
        }
    }

    pub fn filter_row(
        &self,
        row_id: u32,
        subrow_id: Option<u16>,
        row: &ExcelRow<'_>,
        filter: &CompiledFilterInput,
    ) -> anyhow::Result<(bool, bool)> {
        if filter.is_empty() {
            bail!("没有可供匹配的筛选条件");
        }

        let cell_grabber = self.get_cell_grabber(row_id, subrow_id, row);

        let _sw = FILTER_ROW_STOPWATCH.start();
        let mut is_in_progress = false;
        let matches = filter.matches(cell_grabber, &mut is_in_progress, &self.0.filter_cache)?;
        Ok((matches, is_in_progress))
    }

    pub fn score_row(
        &self,
        row_id: u32,
        subrow_id: Option<u16>,
        row: &ExcelRow<'_>,
        filter: &CompiledFilterInput,
    ) -> anyhow::Result<(Option<NonZeroU32>, bool)> {
        if filter.is_empty() {
            bail!("没有可供匹配的筛选条件");
        }

        let cell_grabber = self.get_cell_grabber(row_id, subrow_id, row);

        let mut is_in_progress = false;
        let score = filter.score(cell_grabber, &mut is_in_progress, &self.0.filter_cache)?;
        Ok((score, is_in_progress))
    }

    fn get_cell_grabber<'a>(
        &'a self,
        row_id: u32,
        subrow_id: Option<u16>,
        row: &'a ExcelRow<'_>,
    ) -> impl Fn(&CompiledFilterKey, bool) -> KeyCellIter<'a> {
        let _sw = FILTER_CELL_GRAB_STOPWATCH.start();
        move |key: &CompiledFilterKey, resolve_display_field: bool| -> KeyCellIter<'a> {
            match key {
                CompiledFilterKey::RowId => KeyCellIter::row_id(row_id, subrow_id),
                CompiledFilterKey::RowIdOrColumn(indices) => KeyCellIter::row_id_or_column(
                    self,
                    row_id,
                    subrow_id,
                    *row,
                    indices.clone(),
                    resolve_display_field,
                ),
                CompiledFilterKey::Column(indices, _) => {
                    KeyCellIter::column(self, *row, indices.clone(), resolve_display_field)
                }
            }
        }
    }

    pub fn compile_filter(
        &self,
        input: &FilterInput,
        options: MatchOptions,
    ) -> anyhow::Result<CompiledFilterInput> {
        self.0.filter_cache.compile(input, options)
    }
}
