//! `b4 schema [TABLE]`: list tables and columns with their descriptions.

use crate::cli::{Globals, SchemaArgs};
use crate::db::read::Database;
use crate::output::{self, Cell, Column, Table};
use crate::Error;

/// Describe the tables of the database, or the columns of `args.table`.
///
/// Without a table: one row per table and view with its name, whether it is
/// a table or a view, its number of rows and its description. With a table
/// (or a view; the case of letters does not matter): one row per column with
/// name, type and description. An unknown table is an error that lists the
/// tables there are.
///
/// The descriptions are read back from the database (`COMMENT ON TABLE` /
/// `COMMENT ON COLUMN`, written by `decrypt`), not from the code, so they
/// describe the database at hand. The description is the flexible column.
pub fn run(args: &SchemaArgs, globals: &Globals) -> Result<(), Error> {
    let database = Database::open(&globals.db)?;
    let table = match &args.table {
        Some(name) => columns_of(&database, name)?,
        None => relations_of(&database)?,
    };
    output::print_table(&table, globals)
}

fn relations_of(database: &Database) -> Result<Table, Error> {
    let mut table = Table::new(vec![
        Column::text("name", "Table"),
        Column::text("type", "Type"),
        Column::integer("rows", "Rows"),
        Column::text("description", "Description").flexible(),
    ]);
    for relation in database.relations()? {
        let rows = database.row_count(&relation)?;
        table.push(vec![
            Cell::from(relation.name),
            Cell::from(if relation.is_view { "view" } else { "table" }),
            Cell::Integer(i64::try_from(rows).unwrap_or(i64::MAX)),
            Cell::from(relation.description),
        ]);
    }
    Ok(table)
}

fn columns_of(database: &Database, name: &str) -> Result<Table, Error> {
    let relation = database.relation(name)?;
    let mut table = Table::new(vec![
        Column::text("name", "Column"),
        Column::text("type", "Type"),
        Column::text("description", "Description").flexible(),
    ]);
    for column in database.columns(&relation)? {
        table.push(vec![
            Cell::from(column.name),
            Cell::from(column.data_type),
            Cell::from(column.description),
        ]);
    }
    Ok(table)
}
