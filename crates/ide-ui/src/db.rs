//! Database tool window: data sources, SQL execution and a results grid.
//!
//! Connections and queries go straight to `Project::db` (`ide_db::DbManager`)
//! on the host runtime; this panel owns no SQL logic of its own. SQLite,
//! PostgreSQL, MySQL and SQL Server sources come from the project's
//! `.openidea/data-sources.toml`, the same file the Tauri frontend used.

use crate::theme::IdeColors as _;
use futures::StreamExt;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ide_db::{
    CellValue, ColumnInfo, DataSource, DbDriver, DbRowEdit, DbTableKind, QueryResult, SortKey, TableDetails, TablePage,
};
use ide_host::{Host, Project};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

const MAX_ROWS: usize = 300;
const PAGE_ROWS: u64 = 100;
const CELL_WIDTH: f32 = 150.;
const ROW_HEIGHT: f32 = 20.;

#[derive(Clone, PartialEq, Eq)]
enum Mode {
    Sql,
    Table { name: String, offset: u64, sort: Vec<SortKey> },
}

struct RowEditor {
    /// column name, editor, primary-key, nullable
    fields: Vec<(String, Entity<InputState>, bool, bool)>,
}

struct CellEditor {
    row: usize,
    column: String,
    input: Entity<InputState>,
    _submit: Subscription,
}

pub struct DbPanel {
    host: Arc<Host>,
    project: Option<Arc<Project>>,
    sources: Vec<DataSource>,
    selected: usize,
    tables: Vec<String>,
    views: Vec<String>,
    read_only_grid: bool,
    schemas: Vec<String>,
    current_schema: String,
    expanded_tables: HashSet<String>,
    table_meta: HashMap<String, TableDetails>,
    ddl: Option<(SharedString, SharedString)>,
    sql_input: Entity<InputState>,
    filter_input: Entity<InputState>,
    mode: Mode,
    table_details: Vec<ColumnInfo>,
    row_editor: Option<RowEditor>,
    cell_editor: Option<CellEditor>,
    row_editor_row: Option<usize>,
    row_editor_insert: bool,
    result: Option<QueryResult>,
    message: Option<SharedString>,
    running: bool,
    task: Option<Task<()>>,
    _submit: Subscription,
    focus_handle: FocusHandle,
}

impl DbPanel {
    pub fn new(host: Arc<Host>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sql_input = cx.new(|cx| InputState::new(window, cx).placeholder("SQL, e.g. SELECT * FROM items"));
        let filter_input = cx.new(|cx| InputState::new(window, cx).placeholder("WHERE, e.g. id > 10"));
        let submit = cx.subscribe(&sql_input, |this: &mut Self, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.run(cx);
            }
        });
        cx.subscribe(&filter_input, |this: &mut Self, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.reload_page(cx);
            }
        })
        .detach();
        Self {
            host,
            project: None,
            sources: Vec::new(),
            selected: 0,
            tables: Vec::new(),
            views: Vec::new(),
            read_only_grid: false,
            schemas: Vec::new(),
            current_schema: String::new(),
            expanded_tables: HashSet::new(),
            table_meta: HashMap::new(),
            ddl: None,
            sql_input,
            filter_input,
            mode: Mode::Sql,
            table_details: Vec::new(),
            row_editor: None,
            cell_editor: None,
            row_editor_row: None,
            row_editor_insert: false,
            result: None,
            message: None,
            running: false,
            task: None,
            _submit: submit,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn set_project(&mut self, project: &Arc<Project>, cx: &mut Context<Self>) {
        self.project = Some(project.clone());
        self.sources = project.db.sources().unwrap_or_default();
        self.selected = 0;
        self.tables.clear();
        self.result = None;
        self.mode = Mode::Sql;
        self.table_details.clear();
        self.running = false;
        self.task = None;
        self.message = if self.sources.is_empty() {
            Some("No data sources; add .openidea/data-sources.toml".into())
        } else {
            None
        };
        if !self.sources.is_empty() {
            self.connect(cx);
        }
        cx.notify();
    }

    /// Connect to the selected source and list its tables (run on project open).
    fn connect(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else { return };
        let Some(source) = self.sources.get(self.selected).cloned() else { return };
        let id = source.id.clone();
        let schema = if self.current_schema.is_empty() { Self::schema(&source) } else { self.current_schema.clone() };
        self.current_schema = schema.clone();
        let db = project.db.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        self.message = Some(format!("Connecting to {}…", source.name).into());
        self.host.runtime.spawn(async move {
            let result = async {
                db.pool(&id).await.map_err(|err| err.to_string())?;
                let schemas = db.schemas(&id).await.unwrap_or_default();
                let infos = db.tables(&id, &schema).await.map_err(|err| err.to_string())?;
                let (tables, views) = split_relations(infos);
                Ok::<_, String>((schemas, tables, views))
            }
            .await;
            let _ = tx.unbounded_send(result);
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    match result {
                        Ok((schemas, tables, views)) => {
                            this.schemas = schemas;
                            this.tables = tables;
                            this.views = views;
                            this.message = None;
                        }
                        Err(err) => this.message = Some(err.into()),
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    fn open_ddl(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else { return };
        let Some(source) = self.sources.get(self.selected).cloned() else { return };
        let schema = if self.current_schema.is_empty() { Self::schema(&source) } else { self.current_schema.clone() };
        let db = project.db.clone();
        let id = source.id.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        self.message = Some(format!("Loading DDL for {name}…").into());
        self.host.runtime.spawn(async move {
            let result = db.ddl(&id, &schema, &name).await.map_err(|err| err.to_string());
            let _ = tx.unbounded_send((name, result));
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            while let Some((name, result)) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    match result {
                        Ok(sql) => {
                            this.ddl = Some((format!("DDL · {name}").into(), sql.into()));
                            this.message = None;
                        }
                        Err(err) => this.message = Some(err.into()),
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    fn toggle_table(&mut self, name: String, cx: &mut Context<Self>) {
        if !self.expanded_tables.remove(&name) {
            self.expanded_tables.insert(name.clone());
            if !self.table_meta.contains_key(&name) {
                self.load_columns(name, cx);
            }
        }
        cx.notify();
    }

    fn load_columns(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else { return };
        let Some(source) = self.sources.get(self.selected).cloned() else { return };
        let schema = if self.current_schema.is_empty() { Self::schema(&source) } else { self.current_schema.clone() };
        let db = project.db.clone();
        let id = source.id.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        self.host.runtime.spawn(async move {
            let result = db.table_details(&id, &schema, &name).await.map_err(|err| err.to_string());
            let _ = tx.unbounded_send((name, result));
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            while let Some((name, result)) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    match result {
                        Ok(details) => {
                            this.table_meta.insert(name, details);
                        }
                        Err(err) => this.message = Some(err.into()),
                    }
                    cx.notify();
                });
            }
        }));
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.tables.clear();
        self.expanded_tables.clear();
        self.table_meta.clear();
        self.table_details.clear();
        self.mode = Mode::Sql;
        self.result = None;
        self.connect(cx);
    }

    fn load_tables(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else { return };
        let Some(source) = self.sources.get(self.selected).cloned() else { return };
        let id = source.id.clone();
        let schema = self.current_schema.clone();
        let db = project.db.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        self.host.runtime.spawn(async move {
            let result = db.tables(&id, &schema).await.map(split_relations).map_err(|err| err.to_string());
            let _ = tx.unbounded_send(result);
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    match result {
                        Ok((tables, views)) => {
                            this.tables = tables;
                            this.views = views;
                            this.message = Some(format!("schema {}", this.current_schema).into());
                        }
                        Err(err) => this.message = Some(err.into()),
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    fn select_schema(&mut self, schema: String, cx: &mut Context<Self>) {
        if schema == self.current_schema {
            return;
        }
        self.current_schema = schema;
        self.tables.clear();
        self.views.clear();
        self.expanded_tables.clear();
        self.table_meta.clear();
        self.mode = Mode::Sql;
        self.table_details.clear();
        self.result = None;
        self.load_tables(cx);
    }

    fn schema(source: &DataSource) -> String {
        if !source.options.default_schema.trim().is_empty() {
            return source.options.default_schema.clone();
        }
        match source.driver {
            DbDriver::Sqlite => "main".into(),
            DbDriver::Postgres => "public".into(),
            DbDriver::Mysql => source.database.clone(),
            DbDriver::SqlServer => "dbo".into(),
        }
    }

    fn export_csv(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else { return };
        let Some(result) = self.result.clone() else {
            self.message = Some("Nothing to export".into());
            cx.notify();
            return;
        };
        if result.columns.is_empty() {
            self.message = Some("Nothing to export".into());
            cx.notify();
            return;
        }
        let name = match &self.mode {
            Mode::Table { name, .. } => name.clone(),
            Mode::Sql => "query".to_owned(),
        };
        let safe: String =
            name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
        let stamp =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let dir = project.root.join(".openidea").join("exports");
        let path = dir.join(format!("{safe}-{stamp}.csv"));
        let mut out = String::new();
        let header = result.columns.iter().map(|c| csv_field(&c.name)).collect::<Vec<_>>().join(",");
        out.push_str(&header);
        out.push('\n');
        for row in &result.rows {
            let line = row.iter().map(|value| csv_field(value.as_deref().unwrap_or(""))).collect::<Vec<_>>().join(",");
            out.push_str(&line);
            out.push('\n');
        }
        let outcome = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, out));
        self.message = Some(match outcome {
            Ok(()) => format!("Exported {} rows to {}", result.rows.len(), path.display()).into(),
            Err(err) => format!("Export failed: {err}").into(),
        });
        cx.notify();
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let Some(project) = self.project.clone() else {
            self.message = Some("Open a project first".into());
            cx.notify();
            return;
        };
        let Some(source) = self.sources.get(self.selected).cloned() else {
            self.message = Some("No data source selected".into());
            cx.notify();
            return;
        };
        let sql = self.sql_input.read(cx).value().trim().to_owned();
        if sql.is_empty() {
            self.message = Some("Write a SQL statement first".into());
            cx.notify();
            return;
        }
        self.mode = Mode::Sql;
        self.table_details.clear();
        self.running = true;
        self.message = Some(format!("Running on {}…", source.name).into());
        let id = source.id.clone();
        let schema = Self::schema(&source);
        let db = project.db.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        self.host.runtime.spawn(async move {
            let cancel = ide_core::CancellationToken::new();
            let result = async {
                db.pool(&id).await.map_err(|err| err.to_string())?;
                let result = db.execute(&id, &sql, MAX_ROWS, &cancel).await.map_err(|err| err.to_string())?;
                let tables = db
                    .tables(&id, &schema)
                    .await
                    .map(|tables| tables.into_iter().map(|table| table.name).collect())
                    .unwrap_or_default();
                Ok::<_, String>((result, tables))
            }
            .await;
            let _ = tx.unbounded_send(result);
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    this.running = false;
                    match result {
                        Ok((result, tables)) => {
                            let summary = match result.affected {
                                Some(affected) => format!("{affected} rows affected · {:.0} ms", result.elapsed_ms),
                                None => format!("{} rows · {:.0} ms", result.rows.len(), result.elapsed_ms),
                            };
                            this.message = Some(summary.into());
                            this.result = Some(result);
                            this.tables = tables;
                        }
                        Err(err) => {
                            this.message = Some(err.into());
                            this.result = None;
                        }
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    fn open_table(&mut self, name: String, cx: &mut Context<Self>) {
        self.read_only_grid = false;
        self.mode = Mode::Table { name, offset: 0, sort: Vec::new() };
        self.load_page(cx);
    }

    fn open_view(&mut self, name: String, cx: &mut Context<Self>) {
        self.read_only_grid = true;
        self.mode = Mode::Table { name, offset: 0, sort: Vec::new() };
        self.load_page(cx);
    }

    fn reload_page(&mut self, cx: &mut Context<Self>) {
        let Mode::Table { name, offset, sort } = self.mode.clone() else {
            return;
        };
        let offset = offset.min(self.mode_offset());
        self.mode = Mode::Table { name, offset, sort };
        self.load_page(cx);
    }

    fn mode_offset(&self) -> u64 {
        match &self.mode {
            Mode::Table { offset, .. } => *offset,
            Mode::Sql => 0,
        }
    }

    fn set_page(&mut self, offset: u64, cx: &mut Context<Self>) {
        if let Mode::Table { offset: current, .. } = &mut self.mode {
            *current = offset;
        }
        self.load_page(cx);
    }

    fn open_row_editor(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only_grid {
            self.message = Some("Views are read-only in the table editor".into());
            cx.notify();
            return;
        }
        let Some(result) = self.result.clone() else { return };
        let Some(values) = result.rows.get(row).cloned() else { return };
        let columns: Vec<(String, bool, bool)> = result
            .columns
            .iter()
            .map(|column| {
                let detail = self.table_details.iter().find(|detail| detail.name == column.name);
                (
                    column.name.clone(),
                    detail.is_some_and(|detail| detail.primary_key),
                    detail.is_some_and(|detail| detail.nullable),
                )
            })
            .collect();
        let mut fields = Vec::new();
        for (index, (name, primary_key, nullable)) in columns.into_iter().enumerate() {
            let value = values.get(index).cloned().flatten().unwrap_or_default();
            let input = cx.new(|cx| InputState::new(window, cx).default_value(value));
            fields.push((name, input, primary_key, nullable));
        }
        self.row_editor = Some(RowEditor { fields });
        self.row_editor_row = Some(row);
        self.row_editor_insert = false;
        cx.notify();
    }

    fn open_new_row_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only_grid {
            self.message = Some("Views are read-only in the table editor".into());
            cx.notify();
            return;
        }
        let Mode::Table { name, .. } = self.mode.clone() else { return };
        let mut columns: Vec<(String, bool, bool)> = self
            .table_details
            .iter()
            .map(|column| (column.name.clone(), column.primary_key, column.nullable))
            .collect();
        if columns.is_empty() {
            if let Some(result) = &self.result {
                columns = result.columns.iter().map(|column| (column.name.clone(), false, true)).collect();
            }
        }
        if columns.is_empty() {
            self.message = Some(format!("No columns known for {name}").into());
            cx.notify();
            return;
        }
        let mut fields = Vec::new();
        for (name, primary_key, nullable) in columns {
            let placeholder = if primary_key { "key" } else { "value" };
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
            fields.push((name, input, primary_key, nullable));
        }
        self.row_editor = Some(RowEditor { fields });
        self.row_editor_row = None;
        self.row_editor_insert = true;
        cx.notify();
    }

    fn open_cell_editor(&mut self, row: usize, col: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only_grid {
            self.message = Some("Views are read-only in the table editor".into());
            cx.notify();
            return;
        }
        let Some(result) = self.result.clone() else { return };
        let Some(column) = result.columns.get(col) else { return };
        let value = result.rows.get(row).and_then(|row| row.get(col)).cloned().flatten().unwrap_or_default();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(value));
        let submit = cx.subscribe(&input, |this: &mut Self, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.save_cell_editor(cx);
            }
        });
        self.cell_editor = Some(CellEditor { row, column: column.name.clone(), input, _submit: submit });
        cx.notify();
        let entity = cx.entity();
        window.on_next_frame(move |window, cx| {
            let _ = entity.update(cx, |this, cx| {
                if let Some(editor) = &this.cell_editor {
                    let handle = editor.input.read(cx).focus_handle(cx);
                    window.focus(&handle, cx);
                }
            });
        });
    }

    fn save_cell_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.cell_editor.take() else { return };
        let Some(result) = self.result.clone() else { return };
        let key: Vec<CellValue> = result
            .columns
            .iter()
            .enumerate()
            .filter(|(_, column)| {
                self.table_details.iter().any(|detail| detail.name == column.name && detail.primary_key)
            })
            .map(|(index, column)| CellValue {
                column: column.name.clone(),
                value: result.rows.get(editor.row).and_then(|row| row.get(index)).cloned().flatten(),
            })
            .collect();
        if key.is_empty() {
            self.message = Some("This table has no primary key; cells cannot be edited".into());
            cx.notify();
            return;
        }
        let text = editor.input.read(cx).value().trim().to_owned();
        let nullable =
            self.table_details.iter().find(|column| column.name == editor.column).is_some_and(|column| column.nullable);
        let value =
            CellValue { column: editor.column, value: if text.is_empty() && nullable { None } else { Some(text) } };
        self.apply_row_edit(DbRowEdit::Update { key, values: vec![value] }, cx);
    }

    fn row_key(
        fields: &[(String, Entity<InputState>, bool, bool)],
        result: &QueryResult,
        row: usize,
    ) -> Vec<CellValue> {
        let values = result.rows.get(row).cloned().unwrap_or_default();
        fields
            .iter()
            .enumerate()
            .filter(|(_, (_, _, primary_key, _))| *primary_key)
            .map(|(index, (name, _, _, _))| CellValue {
                column: name.clone(),
                value: values.get(index).cloned().flatten(),
            })
            .collect()
    }

    fn apply_row_edit(&mut self, edit: DbRowEdit, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else { return };
        let Some(source) = self.sources.get(self.selected).cloned() else { return };
        let Mode::Table { name, .. } = self.mode.clone() else { return };
        let schema = if self.current_schema.is_empty() { Self::schema(&source) } else { self.current_schema.clone() };
        let db = project.db.clone();
        let id = source.id.clone();
        self.row_editor = None;
        self.running = true;
        self.message = Some("Applying change…".into());
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        self.host.runtime.spawn(async move {
            let result = db.apply_edits(&id, &schema, &name, &[edit]).await.map_err(|err| err.to_string());
            let _ = tx.unbounded_send(result);
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    this.running = false;
                    match result {
                        Ok(affected) => {
                            this.message = Some(format!("{affected} row(s) changed").into());
                            this.load_page(cx);
                        }
                        Err(err) => this.message = Some(err.into()),
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    fn save_row(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.row_editor.take() else { return };
        if self.row_editor_insert {
            let values: Vec<CellValue> = editor
                .fields
                .iter()
                .filter_map(|(name, input, _, _)| {
                    let text = input.read(cx).value().trim().to_owned();
                    (!text.is_empty()).then_some(CellValue { column: name.clone(), value: Some(text) })
                })
                .collect();
            self.row_editor_insert = false;
            self.apply_row_edit(DbRowEdit::Insert { values }, cx);
            return;
        }
        let Some(result) = self.result.clone() else { return };
        let Some(row) = self.row_editor_row else {
            return;
        };
        let key = Self::row_key(&editor.fields, &result, row);
        if key.is_empty() {
            self.message = Some("This table has no primary key; rows cannot be edited".into());
            cx.notify();
            return;
        }
        let values: Vec<CellValue> = editor
            .fields
            .iter()
            .filter(|(_, _, primary_key, _)| !*primary_key)
            .map(|(name, input, _, nullable)| {
                let text = input.read(cx).value().trim().to_owned();
                CellValue { column: name.clone(), value: if text.is_empty() && *nullable { None } else { Some(text) } }
            })
            .collect();
        self.row_editor_row = None;
        self.apply_row_edit(DbRowEdit::Update { key, values }, cx);
    }

    fn delete_row(&mut self, cx: &mut Context<Self>) {
        if self.row_editor_insert {
            return;
        }
        let Some(editor) = self.row_editor.take() else { return };
        let Some(result) = self.result.clone() else { return };
        let Some(row) = self.row_editor_row else { return };
        let key = Self::row_key(&editor.fields, &result, row);
        if key.is_empty() {
            self.message = Some("This table has no primary key; rows cannot be deleted".into());
            cx.notify();
            return;
        }
        self.row_editor_row = None;
        self.apply_row_edit(DbRowEdit::Delete { key }, cx);
    }

    fn sort_by(&mut self, column: String, cx: &mut Context<Self>) {
        if let Mode::Table { sort, offset, .. } = &mut self.mode {
            if let Some(key) = sort.iter_mut().find(|key| key.column == column) {
                key.descending = !key.descending;
            } else {
                *sort = vec![SortKey { column, descending: false }];
            }
            *offset = 0;
        }
        self.load_page(cx);
    }

    fn load_page(&mut self, cx: &mut Context<Self>) {
        let Mode::Table { name, offset, sort } = self.mode.clone() else {
            return;
        };
        let Some(project) = self.project.clone() else { return };
        let Some(source) = self.sources.get(self.selected).cloned() else {
            self.message = Some("No data source selected".into());
            cx.notify();
            return;
        };
        let filter = self.filter_input.read(cx).value().trim().to_owned();
        let filter = (!filter.is_empty()).then_some(filter);
        let schema = if self.current_schema.is_empty() { Self::schema(&source) } else { self.current_schema.clone() };
        let db = project.db.clone();
        let id = source.id.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        self.running = true;
        self.message = Some(format!("Loading {name}…").into());
        self.host.runtime.spawn(async move {
            let cancel = ide_core::CancellationToken::new();
            let result = db
                .table_page(&id, &schema, &name, filter.as_deref(), &sort, offset, PAGE_ROWS, &cancel)
                .await
                .map_err(|err| err.to_string());
            let _ = tx.unbounded_send(result);
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    this.running = false;
                    match result {
                        Ok(page) => {
                            let TablePage { result, details, offset } = page;
                            this.table_details = details.columns;
                            let summary = format!(
                                "offset {} · {} rows · {:.0} ms",
                                offset as u64,
                                result.rows.len(),
                                result.elapsed_ms
                            );
                            this.message = Some(summary.into());
                            this.result = Some(result);
                        }
                        Err(err) => {
                            this.message = Some(err.into());
                            this.result = None;
                        }
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }
}

fn split_relations(infos: Vec<ide_db::TableInfo>) -> (Vec<String>, Vec<String>) {
    let mut tables = Vec::new();
    let mut views = Vec::new();
    for info in infos {
        if matches!(info.kind, DbTableKind::Table) {
            tables.push(info.name);
        } else {
            views.push(info.name);
        }
    }
    (tables, views)
}

fn csv_field(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

impl Focusable for DbPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DbPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let sources: Vec<(usize, SharedString, bool)> = self
            .sources
            .iter()
            .enumerate()
            .map(|(index, source)| (index, source.name.clone().into(), index == self.selected))
            .collect();
        let tables = self.tables.clone();
        let result = self.result.clone();
        let message = self.message.clone();
        let running = self.running;
        let source_count = self.sources.len();
        let selected_source = self.sources.get(self.selected).map(|source| source.name.clone()).unwrap_or_default();
        let list_theme = theme.clone();
        let hover_bg = theme.list_hover;
        let select_theme = theme.clone();
        let read_only_grid = self.read_only_grid;
        let table_mode = match &self.mode {
            Mode::Table { name, offset, sort } => Some((name.clone(), *offset, sort.clone())),
            Mode::Sql => None,
        };

        let mut source_bar = div()
            .h(px(28.))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(Icon::new(IconName::Database).xsmall().text_color(theme.warning))
            .child(div().text_size(px(11.)).text_color(theme.fg_secondary()).child("Database"));
        for (index, name, active) in sources {
            let select = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                this.selected = index.min(this.sources.len().saturating_sub(1));
                this.tables.clear();
                this.views.clear();
                this.schemas.clear();
                this.current_schema.clear();
                this.expanded_tables.clear();
                this.table_meta.clear();
                this.mode = Mode::Sql;
                this.result = None;
                this.message = None;
                this.connect(cx);
            });
            source_bar = source_bar.child(
                div()
                    .id(ElementId::Name(format!("db-source-{index}").into()))
                    .h(px(20.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .text_size(px(10.5))
                    .whitespace_nowrap()
                    .when(active, |this| this.bg(theme.list_active).text_color(theme.fg_strong()))
                    .when(!active, |this| this.text_color(theme.fg_muted()).hover(move |this| this.bg(hover_bg)))
                    .on_click(select)
                    .child(name),
            );
        }
        let source_bar = source_bar.child(div().flex_1()).child(
            div().text_size(px(10.)).text_color(theme.fg_muted()).child(if source_count == 0 {
                "no sources".to_owned()
            } else {
                format!("{selected_source} · {}", if running { "running" } else { "ready" })
            }),
        );

        let run = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.run(cx));
        let refresh = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.refresh(cx));
        let export = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.export_csv(cx));

        let mut main_col = div().flex_1().min_w_0().flex().flex_col();
        if !tables.is_empty() {
            let mut strip =
                div().h(px(24.)).px_2().flex().items_center().gap_1().border_b_1().border_color(theme.border);
            strip = strip.child(div().text_size(px(9.5)).text_color(theme.fg_muted()).child("TABLES"));
            for table in tables {
                let name = table.clone();
                let pick = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                    this.open_table(name.clone(), cx);
                });
                strip = strip.child(
                    div()
                        .id(ElementId::Name(format!("db-table-{table}").into()))
                        .px_1p5()
                        .py(px(1.))
                        .rounded(px(3.))
                        .cursor_pointer()
                        .text_size(px(10.))
                        .text_color(theme.link)
                        .hover(move |this| this.bg(hover_bg))
                        .on_click(pick)
                        .child(table),
                );
            }
            main_col = main_col.child(strip);
        }
        if let Some((name, offset, sort)) = table_mode.clone() {
            let back = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| {
                this.mode = Mode::Sql;
                this.table_details.clear();
                this.message = Some("SQL mode".into());
                cx.notify();
            });
            let prev = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                this.set_page(offset.saturating_sub(PAGE_ROWS), cx);
            });
            let next = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                this.set_page(offset + PAGE_ROWS, cx);
            });
            let new_row =
                cx.listener(|this: &mut Self, _: &ClickEvent, window, cx| this.open_new_row_editor(window, cx));
            let more = result.as_ref().is_some_and(|result| result.rows.len() as u64 >= PAGE_ROWS);
            let mut row = div()
                .h(px(28.))
                .px_2()
                .flex()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_color(theme.border)
                .child(Icon::new(IconName::Database).xsmall().text_color(theme.info))
                .child(div().text_size(px(11.)).text_color(theme.foreground).child(name.clone()))
                .child(div().w(px(60.)).text_size(px(9.5)).text_color(theme.fg_muted()).child("FILTER"))
                .child(div().flex_1().min_w_0().child(Input::new(&self.filter_input)))
                .when(!read_only_grid, |this| {
                    this.child(
                        div()
                            .id("db-new-row")
                            .h(px(20.))
                            .px_2()
                            .flex()
                            .items_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .border_1()
                            .border_color(theme.border)
                            .hover(move |this| this.bg(hover_bg))
                            .on_click(new_row)
                            .text_size(px(10.))
                            .text_color(theme.fg_secondary())
                            .child("+ Row"),
                    )
                })
                .child(
                    div()
                        .id("db-export-csv")
                        .h(px(20.))
                        .px_2()
                        .flex()
                        .items_center()
                        .rounded(px(3.))
                        .cursor_pointer()
                        .border_1()
                        .border_color(theme.border)
                        .hover(move |this| this.bg(hover_bg))
                        .on_click(export)
                        .text_size(px(10.))
                        .text_color(theme.fg_secondary())
                        .child("CSV"),
                )
                .child(
                    div()
                        .id("db-sql-mode")
                        .h(px(20.))
                        .px_2()
                        .flex()
                        .items_center()
                        .rounded(px(3.))
                        .cursor_pointer()
                        .border_1()
                        .border_color(theme.border)
                        .hover(move |this| this.bg(hover_bg))
                        .on_click(back)
                        .text_size(px(10.))
                        .text_color(theme.link)
                        .child("SQL"),
                )
                .child(
                    div()
                        .id("db-prev")
                        .h(px(20.))
                        .px_2()
                        .flex()
                        .items_center()
                        .rounded(px(3.))
                        .text_size(px(10.))
                        .text_color(if offset > 0 { theme.fg_secondary() } else { theme.fg_muted() })
                        .when(offset > 0, |this| this.cursor_pointer().hover(move |this| this.bg(hover_bg)))
                        .on_click(prev)
                        .child("‹ Prev"),
                )
                .child(div().text_size(px(9.5)).text_color(theme.fg_muted()).child(format!(
                    "{}–{}",
                    offset + 1,
                    offset + result.as_ref().map_or(0, |r| r.rows.len() as u64)
                )))
                .child(
                    div()
                        .id("db-next")
                        .h(px(20.))
                        .px_2()
                        .flex()
                        .items_center()
                        .rounded(px(3.))
                        .text_size(px(10.))
                        .text_color(if more { theme.fg_secondary() } else { theme.fg_muted() })
                        .when(more, |this| this.cursor_pointer().hover(move |this| this.bg(hover_bg)))
                        .on_click(next)
                        .child("Next ›"),
                )
                .when(!sort.is_empty(), |this| {
                    this.child(
                        div().text_size(px(9.5)).text_color(theme.warning).child(format!("sort {}", sort[0].column)),
                    )
                });
            let _ = &mut row;
            main_col = main_col.child(row);
        }
        match result {
            Some(result) => {
                if result.columns.is_empty() {
                    main_col = main_col.child(
                        div()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(12.))
                            .text_color(theme.fg_muted())
                            .child(match result.affected {
                                Some(affected) => format!("{affected} rows affected"),
                                None => "Statement executed".to_owned(),
                            }),
                    );
                } else {
                    let columns: Vec<(String, String)> =
                        result.columns.iter().map(|column| (column.name.clone(), column.type_name.clone())).collect();
                    let mut header = div()
                        .h(px(22.))
                        .w_full()
                        .flex()
                        .items_center()
                        .bg(theme.muted)
                        .border_b_1()
                        .border_color(theme.border);
                    let sorted = table_mode.as_ref().map(|(_, _, sort)| sort.clone()).unwrap_or_default();
                    let header_hover = select_theme.foreground;
                    for (index, (name, type_name)) in columns.iter().enumerate() {
                        let marker = sorted
                            .iter()
                            .find(|key| key.column == *name)
                            .map(|key| if key.descending { " ▼" } else { " ▲" })
                            .unwrap_or("");
                        let mut cell = div()
                            .id(ElementId::Name(format!("db-col-{index}").into()))
                            .w(px(CELL_WIDTH))
                            .flex_shrink_0()
                            .px_2()
                            .truncate()
                            .text_size(px(10.))
                            .text_color(select_theme.fg_secondary())
                            .child(format!("{name}  {type_name}{marker}"));
                        if table_mode.is_some() {
                            let column = name.clone();
                            let sort = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                                this.sort_by(column.clone(), cx);
                            });
                            cell =
                                cell.cursor_pointer().hover(move |this| this.text_color(header_hover)).on_click(sort);
                        }
                        header = header.child(cell);
                    }
                    let rows = result.rows.clone();
                    let editable = table_mode.is_some();
                    let row_hover = list_theme.list_hover;
                    let on_row: Rc<dyn Fn(usize, &mut Window, &mut App)> =
                        Rc::new(cx.processor(|this: &mut Self, index: usize, window, cx| {
                            this.open_row_editor(index, window, cx)
                        }));
                    let on_cell: Rc<dyn Fn((usize, usize), &mut Window, &mut App)> =
                        Rc::new(cx.processor(|this: &mut Self, (row, col): (usize, usize), window, cx| {
                            this.open_cell_editor(row, col, window, cx)
                        }));
                    let row_list = uniform_list("db-rows", rows.len(), move |range, _window, _cx| {
                        range
                            .map(|index| {
                                let row = &rows[index];
                                let mut element = div()
                                    .id(ElementId::Name(format!("db-row-{index}").into()))
                                    .h(px(ROW_HEIGHT))
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .border_b_1()
                                    .border_color(list_theme.border)
                                    .text_size(px(11.));
                                for (col_index, value) in row.iter().enumerate() {
                                    let (text, color) = match value {
                                        Some(value) => (value.clone(), list_theme.foreground),
                                        None => ("NULL".to_owned(), list_theme.fg_muted()),
                                    };
                                    let mut cell = div()
                                        .id(ElementId::Name(format!("db-cell-{index}-{col_index}").into()))
                                        .w(px(CELL_WIDTH))
                                        .flex_shrink_0()
                                        .px_2()
                                        .truncate()
                                        .text_color(color)
                                        .child(text);
                                    if editable {
                                        let on_cell = on_cell.clone();
                                        cell = cell.cursor_pointer().on_mouse_down(
                                            MouseButton::Right,
                                            move |_: &MouseDownEvent, window, cx| {
                                                on_cell((index, col_index), window, cx)
                                            },
                                        );
                                    }
                                    element = element.child(cell);
                                }
                                if editable {
                                    let on_row = on_row.clone();
                                    element = element
                                        .cursor_pointer()
                                        .hover(move |this| this.bg(row_hover))
                                        .on_click(move |_: &ClickEvent, window, cx| on_row(index, window, cx));
                                }
                                element.into_any_element()
                            })
                            .collect()
                    })
                    .size_full();
                    main_col = main_col.child(header).child(div().flex_1().min_h_0().overflow_hidden().child(row_list));
                }
            }
            None => {
                main_col = main_col.child(
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(11.5))
                        .text_color(theme.fg_muted())
                        .child(if source_count == 0 {
                            "Add a data source in .openidea/data-sources.toml"
                        } else {
                            "Write SQL and press Enter to run it"
                        }),
                );
            }
        }

        let mut overlay = None;
        if let Some(editor) = self.row_editor.as_ref() {
            let editor_hover = theme.list_hover;
            let insert = self.row_editor_insert;
            let fields = editor.fields.clone();
            let save = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.save_row(cx));
            let delete = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.delete_row(cx));
            let cancel = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| {
                this.row_editor = None;
                this.row_editor_row = None;
                this.row_editor_insert = false;
                cx.notify();
            });
            let backdrop = cx.listener(|this: &mut Self, _: &MouseDownEvent, _window, cx| {
                this.row_editor = None;
                this.row_editor_row = None;
                this.row_editor_insert = false;
                cx.notify();
            });
            let mut list = div().id("db-row-fields").flex().flex_col().gap_1().max_h(px(250.)).overflow_y_scroll();
            for (name, input, primary_key, _nullable) in fields {
                let mut row = div().h(px(30.)).flex().items_center().gap_2();
                row = row.child(
                    div()
                        .w(px(150.))
                        .truncate()
                        .text_size(px(11.))
                        .text_color(if primary_key { theme.warning } else { theme.fg_secondary() })
                        .child(format!("{name}{}", if primary_key { " (key)" } else { "" })),
                );
                if primary_key && !insert {
                    let value = input.read(cx).value();
                    row = row.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.))
                            .text_color(theme.fg_muted())
                            .child(value),
                    );
                } else {
                    row = row.child(div().flex_1().min_w_0().child(Input::new(&input)));
                }
                list = list.child(row);
            }
            let card = div()
                .w(px(520.))
                .p_4()
                .flex()
                .flex_col()
                .gap_2()
                .bg(theme.popover)
                .border_1()
                .border_color(theme.border_strong())
                .rounded(px(8.))
                .shadow_xl()
                .occlude()
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(if insert { "New row" } else { "Edit row" }),
                )
                .child(div().text_size(px(10.5)).text_color(theme.fg_muted()).child(if insert {
                    "Empty values are skipped, so column defaults and autoincrement apply."
                } else {
                    "Empty nullable values become NULL; primary keys identify the row."
                }))
                .child(list)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .when(!insert, |this| {
                            this.child(
                                div()
                                    .id("db-row-delete")
                                    .h(px(24.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .rounded(px(4.))
                                    .cursor_pointer()
                                    .border_1()
                                    .border_color(theme.border)
                                    .hover(move |this| this.bg(editor_hover))
                                    .on_click(delete)
                                    .text_size(px(11.))
                                    .text_color(theme.danger)
                                    .child("Delete"),
                            )
                        })
                        .child(div().flex_1())
                        .child(
                            div()
                                .id("db-row-cancel")
                                .h(px(24.))
                                .px_3()
                                .flex()
                                .items_center()
                                .rounded(px(4.))
                                .cursor_pointer()
                                .border_1()
                                .border_color(theme.border)
                                .hover(move |this| this.bg(editor_hover))
                                .on_click(cancel)
                                .text_size(px(11.))
                                .text_color(theme.fg_secondary())
                                .child("Cancel"),
                        )
                        .child(
                            div()
                                .id("db-row-save")
                                .h(px(24.))
                                .px_3()
                                .flex()
                                .items_center()
                                .rounded(px(4.))
                                .cursor_pointer()
                                .bg(theme.primary)
                                .hover(move |this| this.opacity(0.9))
                                .on_click(save)
                                .text_size(px(11.))
                                .text_color(theme.primary_foreground)
                                .child(if insert { "Insert" } else { "Save" }),
                        ),
                );
            overlay = Some(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .id("db-row-backdrop")
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .bg(theme.overlay)
                            .on_mouse_down(MouseButton::Left, backdrop),
                    )
                    .child(card)
                    .into_any_element(),
            );
        }

        let mut ddl_overlay = None;
        if let Some((title, sql)) = self.ddl.clone() {
            let ddl_hover = theme.list_hover;
            let close = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| {
                this.ddl = None;
                cx.notify();
            });
            let backdrop = cx.listener(|this: &mut Self, _: &MouseDownEvent, _window, cx| {
                this.ddl = None;
                cx.notify();
            });
            ddl_overlay = Some(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .id("db-ddl-backdrop")
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .bg(theme.overlay)
                            .on_mouse_down(MouseButton::Left, backdrop),
                    )
                    .child(
                        div()
                            .w(px(720.))
                            .max_h(px(520.))
                            .p_4()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .bg(theme.popover)
                            .border_1()
                            .border_color(theme.border_strong())
                            .rounded(px(8.))
                            .shadow_xl()
                            .occlude()
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .id("db-ddl-text")
                                    .flex_1()
                                    .min_h_0()
                                    .max_h(px(400.))
                                    .overflow_y_scroll()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_size(px(10.5))
                                    .text_color(theme.fg_secondary())
                                    .child(sql),
                            )
                            .child(
                                div().flex().justify_end().child(
                                    div()
                                        .id("db-ddl-close")
                                        .h(px(24.))
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .rounded(px(4.))
                                        .cursor_pointer()
                                        .border_1()
                                        .border_color(theme.border)
                                        .hover(move |this| this.bg(ddl_hover))
                                        .on_click(close)
                                        .text_size(px(11.))
                                        .text_color(theme.fg_secondary())
                                        .child("Close"),
                                ),
                            ),
                    )
                    .into_any_element(),
            );
        }
        let mut cell_overlay = None;
        if let Some(editor) = self.cell_editor.as_ref() {
            let cell_hover = theme.list_hover;
            let column = editor.column.clone();
            let input = editor.input.clone();
            let save = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.save_cell_editor(cx));
            let cancel = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| {
                this.cell_editor = None;
                cx.notify();
            });
            let backdrop = cx.listener(|this: &mut Self, _: &MouseDownEvent, _window, cx| {
                this.cell_editor = None;
                cx.notify();
            });
            cell_overlay = Some(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .id("db-cell-backdrop")
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .bg(theme.overlay)
                            .on_mouse_down(MouseButton::Left, backdrop),
                    )
                    .child(
                        div()
                            .w(px(460.))
                            .p_3()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .bg(theme.popover)
                            .border_1()
                            .border_color(theme.border_strong())
                            .rounded(px(8.))
                            .shadow_xl()
                            .occlude()
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child(format!("Edit cell · {column}")),
                            )
                            .child(Input::new(&input))
                            .child(
                                div()
                                    .text_size(px(9.5))
                                    .text_color(theme.fg_muted())
                                    .child("Right-click a cell to edit it; empty nullable values become NULL."),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .justify_end()
                                    .child(
                                        div()
                                            .id("db-cell-cancel")
                                            .h(px(24.))
                                            .px_3()
                                            .flex()
                                            .items_center()
                                            .rounded(px(4.))
                                            .cursor_pointer()
                                            .border_1()
                                            .border_color(theme.border)
                                            .hover(move |this| this.bg(cell_hover))
                                            .on_click(cancel)
                                            .text_size(px(11.))
                                            .text_color(theme.fg_secondary())
                                            .child("Cancel"),
                                    )
                                    .child(
                                        div()
                                            .id("db-cell-save")
                                            .h(px(24.))
                                            .px_3()
                                            .flex()
                                            .items_center()
                                            .rounded(px(4.))
                                            .cursor_pointer()
                                            .bg(theme.primary)
                                            .hover(move |this| this.opacity(0.9))
                                            .on_click(save)
                                            .text_size(px(11.))
                                            .text_color(theme.primary_foreground)
                                            .child("Save"),
                                    ),
                            ),
                    )
                    .into_any_element(),
            );
        }
        let overlay = overlay.or(ddl_overlay).or(cell_overlay);

        let mut sidebar = div()
            .id("db-sidebar")
            .w(px(186.))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(theme.muted)
            .border_r_1()
            .border_color(theme.border)
            .overflow_y_scroll()
            .py_1()
            .child(div().px_2().py(px(2.)).text_size(px(9.)).text_color(theme.fg_muted()).child("SCHEMAS"));
        let current_schema = self.current_schema.clone();
        let schemas = self.schemas.clone();
        let schema_hover = theme.list_hover;
        for schema in schemas {
            let active = schema == current_schema;
            let target = schema.clone();
            let pick = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                this.select_schema(target.clone(), cx);
            });
            sidebar = sidebar.child(
                div()
                    .id(ElementId::Name(format!("db-schema-{schema}").into()))
                    .h(px(20.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1()
                    .cursor_pointer()
                    .text_size(px(10.5))
                    .when(active, |this| this.bg(theme.list_active).text_color(theme.fg_strong()))
                    .when(!active, |this| {
                        this.text_color(theme.fg_secondary()).hover(move |this| this.bg(schema_hover))
                    })
                    .on_click(pick)
                    .child(schema),
            );
        }
        sidebar = sidebar.child(
            div()
                .w_full()
                .px_2()
                .pt_2()
                .pb(px(2.))
                .flex()
                .items_center()
                .child(
                    div()
                        .text_size(px(9.))
                        .text_color(theme.fg_muted())
                        .child(format!("TABLES · {}", self.tables.len())),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .id("db-refresh")
                        .p(px(2.))
                        .rounded(px(2.))
                        .cursor_pointer()
                        .hover(move |this| this.bg(hover_bg))
                        .on_click(refresh)
                        .child(Icon::new(IconName::RefreshCw).xsmall().text_color(theme.fg_muted())),
                ),
        );
        let active_table = table_mode.as_ref().map(|(name, _, _)| name.clone());
        let table_names = self.tables.clone();
        let table_hover = theme.list_hover;
        for table in table_names {
            let active = active_table.as_ref() == Some(&table);
            let expanded = self.expanded_tables.contains(&table);
            let pick_name = table.clone();
            let pick = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                this.open_table(pick_name.clone(), cx);
            });
            let twist_name = table.clone();
            let twist = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                this.toggle_table(twist_name.clone(), cx);
            });
            let ddl_name = table.clone();
            let ddl = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                this.open_ddl(ddl_name.clone(), cx);
            });
            sidebar = sidebar.child(
                div()
                    .id(ElementId::Name(format!("db-side-{table}").into()))
                    .h(px(20.))
                    .w_full()
                    .flex()
                    .items_center()
                    .text_size(px(10.5))
                    .when(active, |this| this.bg(theme.list_active))
                    .when(!active, |this| this.hover(move |this| this.bg(table_hover)))
                    .child(
                        div()
                            .id(ElementId::Name(format!("db-twist-{table}").into()))
                            .w(px(18.))
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .on_click(twist)
                            .child(
                                Icon::new(if expanded { IconName::ChevronDown } else { IconName::ChevronRight })
                                    .xsmall()
                                    .text_color(theme.fg_muted()),
                            ),
                    )
                    .child(
                        div()
                            .id(ElementId::Name(format!("db-open-{table}").into()))
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .pr_2()
                            .flex()
                            .items_center()
                            .gap_1()
                            .cursor_pointer()
                            .text_color(if active { theme.fg_strong() } else { theme.fg_secondary() })
                            .on_click(pick)
                            .child(Icon::new(IconName::Table).xsmall())
                            .child(div().min_w_0().truncate().child(table.clone())),
                    )
                    .child(
                        div()
                            .id(ElementId::Name(format!("db-ddl-{table}").into()))
                            .h(px(16.))
                            .px_1()
                            .mr_1()
                            .flex()
                            .items_center()
                            .rounded(px(2.))
                            .cursor_pointer()
                            .text_size(px(9.))
                            .text_color(theme.link)
                            .hover(move |this| this.bg(table_hover))
                            .on_click(ddl)
                            .child("DDL"),
                    ),
            );
            if expanded {
                match self.table_meta.get(&table).cloned() {
                    Some(meta) => {
                        for column in meta.columns {
                            sidebar = sidebar.child(
                                div()
                                    .h(px(18.))
                                    .w_full()
                                    .pl(px(28.))
                                    .pr_2()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .text_size(px(10.))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(if column.primary_key {
                                                theme.warning
                                            } else {
                                                theme.fg_secondary()
                                            })
                                            .child(column.name.clone()),
                                    )
                                    .when(column.primary_key, |this| {
                                        this.child(
                                            div()
                                                .px_1()
                                                .rounded(px(2.))
                                                .bg(theme.muted)
                                                .text_size(px(8.5))
                                                .text_color(theme.warning)
                                                .child("PK"),
                                        )
                                    })
                                    .child(div().flex_1())
                                    .child(
                                        div()
                                            .max_w(px(90.))
                                            .truncate()
                                            .text_size(px(9.))
                                            .text_color(theme.fg_muted())
                                            .child(column.data_type.clone()),
                                    ),
                            );
                        }
                        if !meta.indexes.is_empty() {
                            sidebar = sidebar.child(
                                div()
                                    .h(px(16.))
                                    .pl(px(28.))
                                    .text_size(px(8.5))
                                    .text_color(theme.fg_muted())
                                    .child("INDEXES"),
                            );
                            for index in meta.indexes {
                                sidebar = sidebar.child(
                                    div()
                                        .h(px(18.))
                                        .w_full()
                                        .pl(px(36.))
                                        .pr_2()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .text_size(px(9.5))
                                        .child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .text_color(theme.fg_secondary())
                                                .child(index.name.clone()),
                                        )
                                        .when(index.unique, |this| {
                                            this.child(
                                                div()
                                                    .px_1()
                                                    .rounded(px(2.))
                                                    .bg(theme.muted)
                                                    .text_size(px(8.))
                                                    .text_color(theme.info)
                                                    .child("UNQ"),
                                            )
                                        })
                                        .child(div().flex_1())
                                        .child(
                                            div()
                                                .max_w(px(110.))
                                                .truncate()
                                                .text_size(px(8.5))
                                                .text_color(theme.fg_muted())
                                                .child(index.columns.join(", ")),
                                        ),
                                );
                            }
                        }
                        if !meta.foreign_keys.is_empty() {
                            sidebar = sidebar.child(
                                div()
                                    .h(px(16.))
                                    .pl(px(28.))
                                    .text_size(px(8.5))
                                    .text_color(theme.fg_muted())
                                    .child("FOREIGN KEYS"),
                            );
                            for fk in meta.foreign_keys {
                                let label = format!(
                                    "{} → {}.{}",
                                    if fk.columns.is_empty() { "?".to_owned() } else { fk.columns.join(", ") },
                                    fk.ref_table,
                                    fk.ref_columns.join(", ")
                                );
                                sidebar = sidebar.child(
                                    div()
                                        .h(px(18.))
                                        .w_full()
                                        .pl(px(36.))
                                        .pr_2()
                                        .flex()
                                        .items_center()
                                        .text_size(px(9.))
                                        .child(
                                            div().min_w_0().truncate().text_color(theme.fg_secondary()).child(label),
                                        ),
                                );
                            }
                        }
                        if !meta.triggers.is_empty() {
                            sidebar = sidebar.child(
                                div()
                                    .h(px(16.))
                                    .pl(px(28.))
                                    .text_size(px(8.5))
                                    .text_color(theme.fg_muted())
                                    .child("TRIGGERS"),
                            );
                            for trigger in meta.triggers {
                                sidebar = sidebar.child(
                                    div()
                                        .h(px(18.))
                                        .w_full()
                                        .pl(px(36.))
                                        .pr_2()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .text_size(px(9.))
                                        .child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .text_color(theme.fg_secondary())
                                                .child(trigger.name.clone()),
                                        )
                                        .child(div().flex_1())
                                        .child(
                                            div()
                                                .truncate()
                                                .text_size(px(8.5))
                                                .text_color(theme.fg_muted())
                                                .child(format!("{} {}", trigger.timing, trigger.events)),
                                        ),
                                );
                            }
                        }
                    }
                    None => {
                        sidebar = sidebar.child(
                            div()
                                .h(px(18.))
                                .pl(px(28.))
                                .text_size(px(9.5))
                                .text_color(theme.fg_muted())
                                .child("loading columns…"),
                        );
                    }
                }
            }
        }
        if !self.views.is_empty() {
            sidebar = sidebar.child(
                div()
                    .w_full()
                    .px_2()
                    .pt_2()
                    .pb(px(2.))
                    .text_size(px(9.))
                    .text_color(theme.fg_muted())
                    .child(format!("VIEWS · {}", self.views.len())),
            );
            let view_names = self.views.clone();
            let view_hover = theme.list_hover;
            let active_view =
                if self.read_only_grid { table_mode.as_ref().map(|(name, _, _)| name.clone()) } else { None };
            for view in view_names {
                let active = active_view.as_ref() == Some(&view);
                let pick_name = view.clone();
                let pick = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                    this.open_view(pick_name.clone(), cx);
                });
                sidebar = sidebar.child(
                    div()
                        .id(ElementId::Name(format!("db-view-{view}").into()))
                        .h(px(20.))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_1()
                        .w_full()
                        .cursor_pointer()
                        .text_size(px(10.5))
                        .when(active, |this| this.bg(theme.list_active).text_color(theme.fg_strong()))
                        .when(!active, |this| {
                            this.text_color(theme.fg_secondary()).hover(move |this| this.bg(view_hover))
                        })
                        .on_click(pick)
                        .child(Icon::new(IconName::Eye).xsmall())
                        .child(div().min_w_0().truncate().child(view)),
                );
            }
        }
        let body = div().flex_1().min_h_0().flex().flex_row().child(sidebar).child(main_col);

        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(source_bar)
            .child(
                div()
                    .px_2()
                    .py_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().flex_1().min_w_0().child(Input::new(&self.sql_input)))
                    .child(
                        div()
                            .id("db-run")
                            .h(px(26.))
                            .px_3()
                            .flex()
                            .items_center()
                            .gap_1()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .bg(theme.primary)
                            .when(running, |this| this.opacity(0.5))
                            .hover(move |this| this.opacity(0.9))
                            .on_click(run)
                            .child(Icon::new(IconName::Play).xsmall().text_color(theme.primary_foreground))
                            .child(div().text_size(px(11.)).text_color(theme.primary_foreground).child("Run")),
                    ),
            )
            .child(body)
            .child(
                div()
                    .h(px(20.))
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_size(px(10.))
                    .text_color(theme.fg_muted())
                    .child(message.unwrap_or_else(|| "Enter runs the statement".into()))
                    .child(format!("limit {MAX_ROWS} rows")),
            )
            .children(overlay)
            .into_any_element()
    }
}
