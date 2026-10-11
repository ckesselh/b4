//! `b4 categories list`: the categories with their booking counts and sums.

use std::collections::BTreeMap;

use rust_decimal::Decimal;

use crate::cli::{CategoriesCommand, CategoriesListArgs, Format, Globals};
use crate::db::read::Database;
use crate::error::Context;
use crate::output::{self, Cell, Column, Table};
use crate::Error;

/// Separates the levels of a category name.
const LEVEL_SEPARATOR: char = ':';

/// Blanks a level is indented by in table output.
const INDENT: usize = 2;

/// Heads the bookings whose category has no record, in table output.
const NOT_IN_CATALOGUE: &str = "(not in catalogue)";

/// Stands for the bookings without a category, in table output.
const UNCATEGORIZED: &str = "(uncategorized)";

/// Run a verb of `b4 categories`.
pub fn run(command: &CategoriesCommand, globals: &Globals) -> Result<(), Error> {
    match command {
        CategoriesCommand::List(args) => list(args, globals),
    }
}

/// `b4 categories list`: print the category tree.
fn list(_args: &CategoriesListArgs, globals: &Globals) -> Result<(), Error> {
    let database = Database::open(&globals.db)?;
    let table = table(&database, globals.format())?;
    output::print_table(&table, globals)
}

/// The category catalogue as a tree, with the counted bookings of every
/// category.
///
/// # The tree
///
/// A node is a record of `categories`, identified by its `category_key`
/// (the full name trimmed and upper-cased, so names that differ only in case
/// are one category; of several records with one key the first stands for
/// all, as in `bookings_view`). Its parent is the node of its `parent_key`.
/// A parent no record exists for still gets a node (`implied_parent`): its
/// name is the child's name up to the last `:`, and so on upwards.
///
/// The children of a node, and the top level, are ordered by `category_key`
/// compared code point by code point. That order depends on no locale and
/// ignores case; digits and punctuation come before letters, and letters
/// outside A to Z, such as umlauts, after Z.
///
/// # The numbers
///
/// Only rows of `bookings` with `counted` are looked at, so a split or batch
/// counts through its parts and never through its master; entries that are
/// not booked yet are included. A booking belongs to the record whose
/// `category_key` equals its own. `booking_count` and `sum` are the bookings
/// of the category itself, `booking_count_with_children` and
/// `sum_with_children` add everything below it.
///
/// Amounts of different currencies are never added. A category has one row
/// per currency of the bookings in it or below it, and a single row without
/// currency and sums when there are none.
///
/// Bookings that no record matches come after the tree: one entry per
/// distinct category text (`not_in_catalogue`; a text that only an implied
/// parent matches is one of them) and one entry for the bookings without a
/// category (`uncategorized`). Over all rows, `booking_count` therefore adds
/// up to the number of counted bookings.
///
/// # The shape
///
/// [`Format::Table`] indents each level by two blanks, shows the last level
/// of the name only, leaves the name out on the second and further currency
/// rows of a category and puts the entries without a record under a
/// `(not in catalogue)` line. The other formats carry no such line and no
/// indentation: every row has the full name, the parent's full name, the
/// depth and the kind of row, and the budget and marker columns of the
/// record.
fn table(database: &Database, format: Format) -> Result<Table, Error> {
    let mut nodes = catalogue(database).context("cannot read the categories")?;
    add_implied_parents(&mut nodes);
    let mut unmatched = Unmatched::default();
    count_bookings(database, &mut nodes, &mut unmatched)
        .context("cannot sum the bookings per category")?;
    add_up_children(&mut nodes)?;

    let mut lines = tree_lines(&nodes);
    lines.extend(unmatched.lines()?);
    Ok(if format == Format::Table {
        text_table(&lines)
    } else {
        data_table(&lines)
    })
}

/// Number and net sum of some bookings in one currency.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Tally {
    bookings: i64,
    sum: Decimal,
}

/// Tallies per currency. Bookings of an account without a currency are
/// tallied under `None`.
type Tallies = BTreeMap<Option<String>, Tally>;

/// Add `from` to `into`, currency by currency.
fn add_tallies(into: &mut Tallies, from: &Tallies) -> Result<(), Error> {
    for (currency, tally) in from {
        let total = into.entry(currency.clone()).or_default();
        let sum = total.sum.checked_add(tally.sum);
        let bookings = total.bookings.checked_add(tally.bookings);
        let (Some(sum), Some(bookings)) = (sum, bookings) else {
            return Err(Error::new(
                "cannot sum the bookings per category: a sum is too large",
            ));
        };
        *total = Tally { bookings, sum };
    }
    Ok(())
}

/// What a row stands for; the `kind` column of the machine formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKind {
    /// A record of the catalogue.
    Category,
    /// A parent that exists only as the first levels of another name.
    ImpliedParent,
    /// The line above the entries without a record. Table output only.
    Group,
    /// A category text of bookings that no record matches.
    NotInCatalogue,
    /// The bookings without a category.
    Uncategorized,
}

impl LineKind {
    fn name(self) -> &'static str {
        match self {
            LineKind::Category => "category",
            LineKind::ImpliedParent => "implied_parent",
            LineKind::Group => "group",
            LineKind::NotInCatalogue => "not_in_catalogue",
            LineKind::Uncategorized => "uncategorized",
        }
    }
}

/// The columns of a `categories` record that are passed through as they are.
#[derive(Debug, Clone, PartialEq)]
struct Details {
    budget_period: Cell,
    budget_amount: Cell,
    special: Cell,
    totals_bucket: Cell,
    hidden_by_banking4: Cell,
}

impl Details {
    fn none() -> Details {
        Details {
            budget_period: Cell::Null,
            budget_amount: Cell::Null,
            special: Cell::Null,
            totals_bucket: Cell::Null,
            hidden_by_banking4: Cell::Null,
        }
    }
}

/// A node of the tree. Its key in the map is the `category_key`.
#[derive(Debug, Clone, PartialEq)]
struct Node {
    full_name: String,
    /// Full name and key of the parent, for a node below the top level.
    parent: Option<(String, String)>,
    /// `None` for an implied parent.
    details: Option<Details>,
    own: Tallies,
    with_children: Tallies,
}

/// One row of the result before it is laid out for a format.
#[derive(Debug, Clone, PartialEq)]
struct Line {
    kind: LineKind,
    /// Level in the tree; what table output indents by.
    level: usize,
    /// What table output shows as the name.
    label: String,
    full_name: Option<String>,
    parent: Option<String>,
    details: Details,
    own: Tallies,
    with_children: Tallies,
}

/// The last level of a full name.
fn last_level(full_name: &str) -> &str {
    full_name
        .rsplit_once(LEVEL_SEPARATOR)
        .map_or(full_name, |(_, last)| last.trim())
}

/// A name or key without its last level; `None` at the top level.
fn without_last_level(name: &str) -> Option<&str> {
    let (parent, _) = name.rsplit_once(LEVEL_SEPARATOR)?;
    let parent = parent.trim();
    (!parent.is_empty()).then_some(parent)
}

fn text(cell: Option<&Cell>) -> Option<String> {
    match cell {
        Some(Cell::Text(text)) => Some(text.clone()),
        _ => None,
    }
}

fn cell(cell: Option<&Cell>) -> Cell {
    cell.cloned().unwrap_or(Cell::Null)
}

/// The records of the catalogue by key: one node per distinct key.
fn catalogue(database: &Database) -> Result<BTreeMap<String, Node>, Error> {
    let rows = database.query(
        "select category_key, full_name, parent, parent_key, budget_period, budget_amount,
                special, totals_bucket, hidden_by_banking4
         from categories
         where category_key is not null
         qualify row_number() over (partition by category_key order by rowid) = 1",
        &[],
    )?;
    let mut nodes = BTreeMap::new();
    for row in rows.rows() {
        let Some(key) = text(row.first()) else {
            continue;
        };
        let full_name = text(row.get(1)).unwrap_or_else(|| key.clone());
        // A parent key is a shorter text than the key of its child. Holding
        // every link to that keeps the tree free of cycles whatever the
        // database says.
        let parent = text(row.get(3))
            .filter(|parent_key| !parent_key.is_empty() && parent_key.len() < key.len())
            .map(|parent_key| {
                let name = text(row.get(2))
                    .or_else(|| without_last_level(&full_name).map(str::to_owned))
                    .unwrap_or_else(|| parent_key.clone());
                (name, parent_key)
            });
        let details = Details {
            budget_period: cell(row.get(4)),
            budget_amount: cell(row.get(5)),
            special: cell(row.get(6)),
            totals_bucket: cell(row.get(7)),
            hidden_by_banking4: cell(row.get(8)),
        };
        nodes.insert(
            key,
            Node {
                full_name,
                parent,
                details: Some(details),
                own: Tallies::new(),
                with_children: Tallies::new(),
            },
        );
    }
    Ok(nodes)
}

/// Give every parent that has no record a node, up to the top level.
///
/// The key of such a node is cut from the key of its child, never made by
/// upper-casing the name here: the database upper-cased the keys, and two
/// implementations of that do not agree on every letter.
fn add_implied_parents(nodes: &mut BTreeMap<String, Node>) {
    let mut missing: Vec<(String, String)> = nodes
        .values()
        .filter_map(|node| node.parent.clone())
        .collect();
    while let Some((full_name, key)) = missing.pop() {
        if nodes.contains_key(&key) {
            continue;
        }
        let parent = without_last_level(&key).map(|parent_key| {
            let name = without_last_level(&full_name).unwrap_or(parent_key);
            (name.to_owned(), parent_key.to_owned())
        });
        missing.extend(parent.clone());
        nodes.insert(
            key,
            Node {
                full_name,
                parent,
                details: None,
                own: Tallies::new(),
                with_children: Tallies::new(),
            },
        );
    }
}

/// The counted bookings that no record of the catalogue matches.
#[derive(Debug, Default)]
struct Unmatched {
    /// By `category_key`: the category as a booking spells it, and the
    /// tallies.
    not_in_catalogue: BTreeMap<String, (String, Tallies)>,
    uncategorized: Tallies,
}

impl Unmatched {
    /// The lines after the tree: the group with one entry per category text,
    /// ordered by key like the tree, then the bookings without a category.
    /// Nothing for what there are no bookings of.
    fn lines(self) -> Result<Vec<Line>, Error> {
        let mut lines = Vec::new();
        if !self.not_in_catalogue.is_empty() {
            let mut all = Tallies::new();
            for (_, tallies) in self.not_in_catalogue.values() {
                add_tallies(&mut all, tallies)?;
            }
            lines.push(Line {
                kind: LineKind::Group,
                level: 0,
                label: NOT_IN_CATALOGUE.to_owned(),
                full_name: None,
                parent: None,
                details: Details::none(),
                own: Tallies::new(),
                with_children: all,
            });
        }
        for (category, tallies) in self.not_in_catalogue.into_values() {
            lines.push(Line {
                kind: LineKind::NotInCatalogue,
                level: 1,
                label: category.clone(),
                full_name: Some(category),
                parent: None,
                details: Details::none(),
                own: tallies.clone(),
                with_children: tallies,
            });
        }
        if !self.uncategorized.is_empty() {
            lines.push(Line {
                kind: LineKind::Uncategorized,
                level: 0,
                label: UNCATEGORIZED.to_owned(),
                full_name: None,
                parent: None,
                details: Details::none(),
                own: self.uncategorized.clone(),
                with_children: self.uncategorized,
            });
        }
        Ok(lines)
    }
}

/// Tally the counted bookings per category and currency: into the node of
/// the record they match, or into `unmatched`.
fn count_bookings(
    database: &Database,
    nodes: &mut BTreeMap<String, Node>,
    unmatched: &mut Unmatched,
) -> Result<(), Error> {
    let rows = database.query(
        "select category_key, min(category), currency, count(*), sum(amount)
         from bookings
         where counted
         group by category_key, currency
         order by category_key, currency",
        &[],
    )?;
    for row in rows.rows() {
        let key = text(row.first());
        let currency = text(row.get(2));
        let bookings = match row.get(3) {
            Some(Cell::Integer(count)) => *count,
            _ => 0,
        };
        let sum = match row.get(4) {
            Some(Cell::Decimal(sum)) => *sum,
            Some(Cell::Null) | None => Decimal::ZERO,
            Some(other) => {
                return Err(Error::new(format!(
                    "the sum of a category is not a decimal number: {other:?}"
                )));
            }
        };
        let tallies = match key {
            None => &mut unmatched.uncategorized,
            Some(key) => match nodes.get_mut(&key) {
                Some(Node {
                    details: Some(_),
                    own,
                    ..
                }) => own,
                _ => {
                    let category = text(row.get(1)).unwrap_or_else(|| key.clone());
                    let entry = unmatched.not_in_catalogue.entry(key);
                    &mut entry.or_insert_with(|| (category, Tallies::new())).1
                }
            },
        };
        let one = Tallies::from([(currency, Tally { bookings, sum })]);
        add_tallies(tallies, &one)?;
    }
    Ok(())
}

/// Fill `with_children` of every node: its own tallies plus those of
/// everything below it.
fn add_up_children(nodes: &mut BTreeMap<String, Node>) -> Result<(), Error> {
    for node in nodes.values_mut() {
        node.with_children = node.own.clone();
    }
    // A child's key is longer than its parent's, so going from the longest
    // key to the shortest finishes every node before it is added to its
    // parent.
    let mut keys: Vec<String> = nodes.keys().cloned().collect();
    keys.sort_by_key(|key| std::cmp::Reverse(key.len()));
    for key in keys {
        let Some(node) = nodes.get(&key) else {
            continue;
        };
        let Some((_, parent_key)) = node.parent.clone() else {
            continue;
        };
        let tallies = node.with_children.clone();
        if let Some(parent) = nodes.get_mut(&parent_key) {
            add_tallies(&mut parent.with_children, &tallies)?;
        }
    }
    Ok(())
}

/// The nodes as lines: every parent directly before its children, siblings
/// in the order of their keys.
fn tree_lines(nodes: &BTreeMap<String, Node>) -> Vec<Line> {
    // The map is ordered by key, so every list of children is too.
    let mut children: BTreeMap<Option<&str>, Vec<&str>> = BTreeMap::new();
    for (key, node) in nodes {
        let parent = node
            .parent
            .as_ref()
            .map(|(_, parent_key)| parent_key.as_str())
            .filter(|parent_key| nodes.contains_key(*parent_key));
        children.entry(parent).or_default().push(key);
    }

    let mut lines = Vec::with_capacity(nodes.len());
    let mut pending: Vec<(&str, usize)> = below(&children, None).map(|key| (key, 0)).collect();
    while let Some((key, level)) = pending.pop() {
        let Some(node) = nodes.get(key) else {
            continue;
        };
        lines.push(Line {
            kind: if node.details.is_some() {
                LineKind::Category
            } else {
                LineKind::ImpliedParent
            },
            level,
            label: last_level(&node.full_name).to_owned(),
            full_name: Some(node.full_name.clone()),
            parent: node.parent.as_ref().map(|(name, _)| name.clone()),
            details: node.details.clone().unwrap_or_else(Details::none),
            own: node.own.clone(),
            with_children: node.with_children.clone(),
        });
        pending.extend(below(&children, Some(key)).map(|child| (child, level.saturating_add(1))));
    }
    lines
}

/// The children of `key` (the top level for `None`), last first: the order
/// to put them on a stack in.
fn below<'a>(
    children: &'a BTreeMap<Option<&'a str>, Vec<&'a str>>,
    key: Option<&'a str>,
) -> impl Iterator<Item = &'a str> {
    children.get(&key).into_iter().flatten().rev().copied()
}

/// The currency and the four number cells of a line for every currency it has, or once
/// without a currency when it has no bookings: currency, bookings, sum,
/// bookings with children, sum with children.
fn number_rows(line: &Line) -> Vec<[Cell; 5]> {
    if line.with_children.is_empty() {
        return vec![[
            Cell::Null,
            Cell::Integer(0),
            Cell::Null,
            Cell::Integer(0),
            Cell::Null,
        ]];
    }
    line.with_children
        .iter()
        .map(|(currency, total)| {
            let own = line.own.get(currency);
            [
                Cell::from(currency.clone()),
                Cell::Integer(own.map_or(0, |tally| tally.bookings)),
                own.map_or(Cell::Null, |tally| Cell::Decimal(tally.sum)),
                Cell::Integer(total.bookings),
                Cell::Decimal(total.sum),
            ]
        })
        .collect()
}

/// What table output says about a line in its last column.
fn note(line: &Line) -> Cell {
    let mut notes: Vec<&str> = Vec::new();
    if line.kind == LineKind::ImpliedParent {
        notes.push("no record");
    }
    match &line.details.special {
        Cell::Text(special) if special != "none" => notes.push(special),
        _ => {}
    }
    if line.details.hidden_by_banking4 == Cell::Boolean(true) {
        notes.push("hidden by Banking4");
    }
    if notes.is_empty() {
        Cell::Null
    } else {
        Cell::from(notes.join(", "))
    }
}

/// The lines as a tree for people.
fn text_table(lines: &[Line]) -> Table {
    let mut table = Table::new(vec![
        Column::text("category", "Category"),
        Column::integer("booking_count", "Bookings"),
        Column::money("sum", "Sum", "currency"),
        Column::text("currency", "Currency"),
        Column::integer("booking_count_with_children", "With children"),
        Column::money("sum_with_children", "Sum with children", "currency"),
        Column::text("note", "Note"),
    ]);
    for line in lines {
        let name = format!("{}{}", " ".repeat(line.level * INDENT), line.label);
        for (index, numbers) in number_rows(line).into_iter().enumerate() {
            let [currency, bookings, sum, bookings_with_children, sum_with_children] = numbers;
            let first = index == 0;
            table.push(vec![
                if first {
                    Cell::from(name.as_str())
                } else {
                    Cell::Null
                },
                bookings,
                sum,
                currency,
                bookings_with_children,
                sum_with_children,
                if first { note(line) } else { Cell::Null },
            ]);
        }
    }
    table
}

/// The lines as data: no group line, no indentation, every column.
fn data_table(lines: &[Line]) -> Table {
    let mut table = Table::new(vec![
        Column::text("full_name", "Category"),
        Column::text("name", "Name"),
        Column::text("parent", "Parent"),
        Column::integer("depth", "Depth"),
        Column::text("kind", "Kind"),
        Column::text("currency", "Currency"),
        Column::integer("booking_count", "Bookings"),
        Column::money("sum", "Sum", "currency"),
        Column::integer("booking_count_with_children", "With children"),
        Column::money("sum_with_children", "Sum with children", "currency"),
        Column::text("budget_period", "Budget period"),
        Column::decimal("budget_amount", "Budget", 2),
        Column::text("special", "Special"),
        Column::text("totals_bucket", "Totals bucket"),
        Column::boolean("hidden_by_banking4", "Hidden by Banking4"),
    ]);
    for line in lines.iter().filter(|line| line.kind != LineKind::Group) {
        // Entries without a record are not part of the tree.
        let in_tree = matches!(line.kind, LineKind::Category | LineKind::ImpliedParent);
        let depth = if in_tree { line.level } else { 0 };
        for numbers in number_rows(line) {
            let [currency, bookings, sum, bookings_with_children, sum_with_children] = numbers;
            table.push(vec![
                Cell::from(line.full_name.clone()),
                Cell::from(line.full_name.as_deref().map(|name| {
                    if in_tree {
                        last_level(name)
                    } else {
                        name
                    }
                })),
                Cell::from(line.parent.clone()),
                Cell::Integer(i64::try_from(depth).unwrap_or(i64::MAX)),
                Cell::from(line.kind.name()),
                currency,
                bookings,
                sum,
                bookings_with_children,
                sum_with_children,
                line.details.budget_period.clone(),
                line.details.budget_amount.clone(),
                line.details.special.clone(),
                line.details.totals_bucket.clone(),
                line.details.hidden_by_banking4.clone(),
            ]);
        }
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema;
    use crate::db::SCHEMA_VERSION;
    use crate::output::Options;

    /// A database of the real schema holding `sql`'s rows.
    fn database(sql: &str) -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b4.duckdb");
        let connection = duckdb::Connection::open(&path).unwrap();
        schema::create(&connection).unwrap();
        connection
            .execute_batch(&format!(
                "insert into meta values ('schema_version', '{SCHEMA_VERSION}'); {sql}"
            ))
            .unwrap();
        drop(connection);
        let database = Database::open(&path).unwrap();
        (dir, database)
    }

    /// Three levels, two implied parents (one of them two levels deep), a
    /// category without bookings, a special one, a record whose key repeats
    /// another and one without a name; bookings in two currencies, a split,
    /// categories missing from the catalogue and uncategorised bookings.
    fn sample() -> (tempfile::TempDir, Database) {
        database(
            "insert into categories
               (id, full_name, category_key, name, parent, parent_key, depth, special,
                totals_bucket, budget_period, budget_amount, hidden_by_banking4) values
               (1, 'Wohnen', 'WOHNEN', 'Wohnen', null, null, 0, 'none', 'expense', 'month', 1200, false),
               (2, 'Wohnen:Nebenkosten', 'WOHNEN:NEBENKOSTEN', 'Nebenkosten', 'Wohnen', 'WOHNEN', 1, 'none', 'expense', 'none', 0, false),
               (3, 'Wohnen:Nebenkosten:Strom', 'WOHNEN:NEBENKOSTEN:STROM', 'Strom', 'Wohnen:Nebenkosten', 'WOHNEN:NEBENKOSTEN', 2, 'none', 'expense', 'none', 0, false),
               (4, 'Wohnen:Miete', 'WOHNEN:MIETE', 'Miete', 'Wohnen', 'WOHNEN', 1, 'none', 'expense', 'month', 950, false),
               (5, 'Freizeit:Sport', 'FREIZEIT:SPORT', 'Sport', 'Freizeit', 'FREIZEIT', 1, 'none', 'expense', 'none', 0, false),
               (6, 'Äpfel:Bio:Elstar', 'ÄPFEL:BIO:ELSTAR', 'Elstar', 'Äpfel:Bio', 'ÄPFEL:BIO', 2, 'none', 'auto', 'none', 0, false),
               (7, 'Leer', 'LEER', 'Leer', null, null, 0, 'none', 'auto', 'none', 0, true),
               (8, '(Umbuchung)', '(UMBUCHUNG)', '(Umbuchung)', null, null, 0, 'reclassification', 'exclude', 'none', 0, false),
               (9, 'auto', 'AUTO', 'auto', null, null, 0, 'none', 'expense', 'none', 0, false),
               (10, 'WOHNEN', 'WOHNEN', 'WOHNEN', null, null, 0, 'excluded', 'exclude', 'year', 1, false),
               (11, null, null, null, null, null, 0, 'none', 'auto', 'none', 0, false);
             insert into bookings
               (account_id, id, category, category_key, amount, currency, counted, split_role, pending) values
               (1, 1, 'Wohnen', 'WOHNEN', -10, 'EUR', true, null, false),
               (1, 2, 'Wohnen:Miete', 'WOHNEN:MIETE', -950, 'EUR', true, null, false),
               (1, 3, 'wohnen:miete', 'WOHNEN:MIETE', -950.5, 'EUR', true, null, true),
               (1, 4, 'Wohnen:Nebenkosten:Strom', 'WOHNEN:NEBENKOSTEN:STROM', -60, 'EUR', true, null, false),
               (2, 4, 'Wohnen:Nebenkosten:Strom', 'WOHNEN:NEBENKOSTEN:STROM', -5.25, 'USD', true, null, false),
               (1, 10, 'Wohnen:Nebenkosten', 'WOHNEN:NEBENKOSTEN', -100, 'EUR', false, 'split_master', false),
               (1, 11, 'Wohnen:Nebenkosten:Strom', 'WOHNEN:NEBENKOSTEN:STROM', -70, 'EUR', true, 'split_slave', false),
               (1, 12, 'Wohnen:Nebenkosten', 'WOHNEN:NEBENKOSTEN', -30, 'EUR', true, 'split_slave', false),
               (1, 20, 'Freizeit:Sport', 'FREIZEIT:SPORT', -20, 'EUR', true, null, false),
               (1, 21, 'Freizeit', 'FREIZEIT', -7, 'EUR', true, null, false),
               (1, 22, 'Hobby:Angeln', 'HOBBY:ANGELN', -15, 'EUR', true, null, false),
               (1, 23, 'hobby:angeln', 'HOBBY:ANGELN', -5, 'EUR', true, null, false),
               (2, 24, 'Hobby:Angeln', 'HOBBY:ANGELN', -3, 'USD', true, null, false),
               (1, 25, 'Äpfel:Bio:Elstar', 'ÄPFEL:BIO:ELSTAR', -2.5, 'EUR', true, null, false),
               (1, 30, '(Umbuchung)', '(UMBUCHUNG)', 100, 'EUR', true, null, false),
               (1, 31, '(Umbuchung)', '(UMBUCHUNG)', -100, 'EUR', true, null, false),
               (1, 32, 'auto', 'AUTO', -1, 'EUR', true, null, false),
               (1, 40, null, null, -4, 'EUR', true, null, false),
               (1, 41, null, null, 9, 'EUR', true, null, false),
               (2, 42, null, null, -8, 'USD', true, null, false),
               (1, 43, null, null, -1000, 'EUR', false, 'batch_master', false);",
        )
    }

    fn rendered(table: &Table, format: Format) -> String {
        let options = Options {
            format,
            header: true,
            width: None,
        };
        let mut out = Vec::new();
        output::render_table(&mut out, table, &options).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn index(table: &Table, name: &str) -> usize {
        table
            .columns()
            .iter()
            .position(|column| column.name() == name)
            .unwrap_or_else(|| panic!("no column {name}"))
    }

    fn text_of(table: &Table, row: &[Cell], name: &str) -> Option<String> {
        match &row[index(table, name)] {
            Cell::Text(text) => Some(text.clone()),
            Cell::Null => None,
            other => panic!("{name} is {other:?}"),
        }
    }

    fn integer_of(table: &Table, row: &[Cell], name: &str) -> i64 {
        match &row[index(table, name)] {
            Cell::Integer(value) => *value,
            other => panic!("{name} is {other:?}"),
        }
    }

    #[test]
    fn the_table_is_a_tree_with_own_and_inherited_numbers() {
        let (_dir, database) = sample();
        let tree = table(&database, Format::Table).unwrap();
        // Siblings by key: `(` before letters, `auto` among the capitals,
        // `Ä` after `W`. The second `WOHNEN` record and the nameless record
        // are no nodes; the split counts through its two parts.
        let expected = "\
CATEGORY            BOOKINGS           SUM  WITH CHILDREN  SUM WITH CHILDREN  NOTE
(Umbuchung)                2      0.00 EUR              2           0.00 EUR  reclassification
auto                       1     -1.00 EUR              1          -1.00 EUR
Freizeit                   0                            1         -20.00 EUR  no record
  Sport                    1    -20.00 EUR              1         -20.00 EUR
Leer                       0                            0                     hidden by Banking4
Wohnen                     1    -10.00 EUR              6       -2070.50 EUR
                           0                            1          -5.25 USD
  Miete                    2  -1900.50 EUR              2       -1900.50 EUR
  Nebenkosten              1    -30.00 EUR              3        -160.00 EUR
                           0                            1          -5.25 USD
    Strom                  2   -130.00 EUR              2        -130.00 EUR
                           1     -5.25 USD              1          -5.25 USD
Äpfel                      0                            1          -2.50 EUR  no record
  Bio                      0                            1          -2.50 EUR  no record
    Elstar                 1     -2.50 EUR              1          -2.50 EUR
(not in catalogue)         0                            3         -27.00 EUR
                           0                            1          -3.00 USD
  Freizeit                 1     -7.00 EUR              1          -7.00 EUR
  Hobby:Angeln             2    -20.00 EUR              2         -20.00 EUR
                           1     -3.00 USD              1          -3.00 USD
(uncategorized)            2      5.00 EUR              2           5.00 EUR
                           1     -8.00 USD              1          -8.00 USD
";
        assert_eq!(rendered(&tree, Format::Table), expected);
    }

    #[test]
    fn the_other_formats_have_one_flat_row_per_category_and_currency() {
        let (_dir, database) = sample();
        let data = table(&database, Format::Csv).unwrap();
        let expected = "\
full_name,name,parent,depth,kind,currency,booking_count,sum,booking_count_with_children,sum_with_children,budget_period,budget_amount,special,totals_bucket,hidden_by_banking4
(Umbuchung),(Umbuchung),,0,category,EUR,2,0.00,2,0.00,none,0,reclassification,exclude,false
auto,auto,,0,category,EUR,1,-1.00,1,-1.00,none,0,none,expense,false
Freizeit,Freizeit,,0,implied_parent,EUR,0,,1,-20.00,,,,,
Freizeit:Sport,Sport,Freizeit,1,category,EUR,1,-20.00,1,-20.00,none,0,none,expense,false
Leer,Leer,,0,category,,0,,0,,none,0,none,auto,true
Wohnen,Wohnen,,0,category,EUR,1,-10.00,6,-2070.50,month,1200,none,expense,false
Wohnen,Wohnen,,0,category,USD,0,,1,-5.25,month,1200,none,expense,false
Wohnen:Miete,Miete,Wohnen,1,category,EUR,2,-1900.50,2,-1900.50,month,950,none,expense,false
Wohnen:Nebenkosten,Nebenkosten,Wohnen,1,category,EUR,1,-30.00,3,-160.00,none,0,none,expense,false
Wohnen:Nebenkosten,Nebenkosten,Wohnen,1,category,USD,0,,1,-5.25,none,0,none,expense,false
Wohnen:Nebenkosten:Strom,Strom,Wohnen:Nebenkosten,2,category,EUR,2,-130.00,2,-130.00,none,0,none,expense,false
Wohnen:Nebenkosten:Strom,Strom,Wohnen:Nebenkosten,2,category,USD,1,-5.25,1,-5.25,none,0,none,expense,false
Äpfel,Äpfel,,0,implied_parent,EUR,0,,1,-2.50,,,,,
Äpfel:Bio,Bio,Äpfel,1,implied_parent,EUR,0,,1,-2.50,,,,,
Äpfel:Bio:Elstar,Elstar,Äpfel:Bio,2,category,EUR,1,-2.50,1,-2.50,none,0,none,auto,false
Freizeit,Freizeit,,0,not_in_catalogue,EUR,1,-7.00,1,-7.00,,,,,
Hobby:Angeln,Hobby:Angeln,,0,not_in_catalogue,EUR,2,-20.00,2,-20.00,,,,,
Hobby:Angeln,Hobby:Angeln,,0,not_in_catalogue,USD,1,-3.00,1,-3.00,,,,,
,,,0,uncategorized,EUR,2,5.00,2,5.00,,,,,
,,,0,uncategorized,USD,1,-8.00,1,-8.00,,,,,
";
        assert_eq!(rendered(&data, Format::Csv), expected);

        // json and jsonl are the same rows.
        for format in [Format::Json, Format::Jsonl] {
            assert_eq!(table(&database, format).unwrap(), data);
        }
        let lines = rendered(&data, Format::Jsonl);
        assert_eq!(
            lines.lines().nth(6).unwrap(),
            "{\"full_name\":\"Wohnen\",\"name\":\"Wohnen\",\"parent\":null,\"depth\":0,\
             \"kind\":\"category\",\"currency\":\"USD\",\"booking_count\":0,\"sum\":null,\
             \"booking_count_with_children\":1,\"sum_with_children\":-5.25,\
             \"budget_period\":\"month\",\"budget_amount\":1200,\"special\":\"none\",\
             \"totals_bucket\":\"expense\",\"hidden_by_banking4\":false}"
        );
        assert!(rendered(&data, Format::Json).starts_with("[\n  {\"full_name\":\"(Umbuchung)\""));
    }

    #[test]
    fn the_own_counts_add_up_to_all_counted_bookings() {
        let (_dir, database) = sample();
        let counted = database
            .query("select count(*) from bookings where counted", &[])
            .unwrap();
        assert_eq!(counted.rows()[0][0], Cell::Integer(19));
        for format in [Format::Table, Format::Json] {
            let table = table(&database, format).unwrap();
            let own: i64 = table
                .rows()
                .iter()
                .map(|row| integer_of(&table, row, "booking_count"))
                .sum();
            assert_eq!(own, 19, "{format:?}");
        }
        // The top level of the tree and the entries after it hold everything
        // once more.
        let data = table(&database, Format::Json).unwrap();
        let top: i64 = data
            .rows()
            .iter()
            .filter(|row| integer_of(&data, row, "depth") == 0)
            .map(|row| integer_of(&data, row, "booking_count_with_children"))
            .sum();
        assert_eq!(top, 19);
    }

    #[test]
    fn every_parent_named_by_a_row_is_a_row() {
        let (_dir, database) = sample();
        let data = table(&database, Format::Json).unwrap();
        let names: Vec<Option<String>> = data
            .rows()
            .iter()
            .filter(|row| text_of(&data, row, "kind").as_deref() != Some("not_in_catalogue"))
            .map(|row| text_of(&data, row, "full_name"))
            .collect();
        for row in data.rows() {
            let parent = text_of(&data, row, "parent");
            if parent.is_some() {
                assert!(names.contains(&parent), "{parent:?}");
            }
        }
    }

    #[test]
    fn an_empty_database_has_no_rows_and_a_bare_catalogue_no_sums() {
        let (_dir, empty) = database("");
        assert!(table(&empty, Format::Table).unwrap().is_empty());
        assert_eq!(
            rendered(&table(&empty, Format::Json).unwrap(), Format::Json),
            "[]\n"
        );

        let (_dir, bare) = database(
            "insert into categories (id, full_name, category_key, parent, parent_key, special) values
               (1, 'B', 'B', null, null, 'none'), (2, 'A:Z', 'A:Z', 'A', 'A', 'vat');",
        );
        assert_eq!(
            rendered(&table(&bare, Format::Table).unwrap(), Format::Table),
            "CATEGORY  BOOKINGS  SUM  WITH CHILDREN  SUM WITH CHILDREN  NOTE\n\
             A                0                   0                     no record\n\
             \x20 Z              0                   0                     vat\n\
             B                0                   0\n"
        );
    }

    #[test]
    fn uncategorised_bookings_alone_are_one_row() {
        let (_dir, database) = database(
            "insert into bookings (account_id, id, amount, currency, counted) values
               (1, 1, -1.5, 'EUR', true), (1, 2, -2, 'EUR', true), (1, 3, -9, 'EUR', false),
               (2, 1, 4, null, true);",
        );
        assert_eq!(
            rendered(&table(&database, Format::Table).unwrap(), Format::Table),
            // An amount without a currency keeps its decimal point in line.
            "CATEGORY         BOOKINGS        SUM  WITH CHILDREN  SUM WITH CHILDREN  NOTE\n\
             (uncategorized)         1   4.00                  1           4.00\n\
             \x20                       2  -3.50 EUR              2          -3.50 EUR\n"
        );
    }

    #[test]
    fn a_parent_link_that_is_no_shorter_key_makes_a_top_level_category() {
        // Two records that name each other as parent, and one that names
        // itself: no database b4 writes has them, and none may hang the
        // command or lose a category.
        let (_dir, database) = database(
            "insert into categories (id, full_name, category_key, parent, parent_key) values
               (1, 'A:B', 'A:B', 'C:D', 'C:D'), (2, 'C:D', 'C:D', 'A:B', 'A:B'),
               (3, 'E', 'E', 'E', 'E'), (4, 'F:G', 'F:G', null, 'F');
             insert into bookings (account_id, id, category, category_key, amount, currency, counted) values
               (1, 1, 'A:B', 'A:B', 1, 'EUR', true), (1, 2, 'C:D', 'C:D', 2, 'EUR', true),
               (1, 3, 'E', 'E', 3, 'EUR', true), (1, 4, 'F:G', 'F:G', 4, 'EUR', true);",
        );
        let data = table(&database, Format::Json).unwrap();
        let rows: Vec<(String, Option<String>, i64, i64)> = data
            .rows()
            .iter()
            .map(|row| {
                (
                    text_of(&data, row, "full_name").unwrap(),
                    text_of(&data, row, "parent"),
                    integer_of(&data, row, "depth"),
                    integer_of(&data, row, "booking_count_with_children"),
                )
            })
            .collect();
        let row = |name: &str, parent: Option<&str>, depth, bookings| {
            (name.to_owned(), parent.map(str::to_owned), depth, bookings)
        };
        assert_eq!(
            rows,
            [
                row("A:B", None, 0, 1),
                row("C:D", None, 0, 1),
                row("E", None, 0, 1),
                // The name of the parent is taken from the child's when the
                // record gives none.
                row("F", None, 0, 1),
                row("F:G", Some("F"), 1, 1),
            ]
        );
    }

    #[test]
    fn levels_are_cut_at_colons_and_trimmed() {
        assert_eq!(last_level("Wohnen"), "Wohnen");
        assert_eq!(last_level("Wohnen:Nebenkosten: Strom"), "Strom");
        assert_eq!(without_last_level("Wohnen"), None);
        assert_eq!(without_last_level(":Wohnen"), None);
        assert_eq!(
            without_last_level("Wohnen:Nebenkosten :Strom"),
            Some("Wohnen:Nebenkosten")
        );
    }

    #[test]
    fn amounts_of_one_currency_are_added_and_others_kept_apart() {
        let eur = |bookings, sum: i64| {
            (
                Some("EUR".to_owned()),
                Tally {
                    bookings,
                    sum: Decimal::new(sum, 2),
                },
            )
        };
        let mut into = Tallies::from([eur(1, 150)]);
        let from = Tallies::from([
            eur(2, -50),
            (
                None,
                Tally {
                    bookings: 1,
                    sum: Decimal::ONE,
                },
            ),
        ]);
        add_tallies(&mut into, &from).unwrap();
        assert_eq!(into.len(), 2);
        assert_eq!(into[&Some("EUR".to_owned())], eur(3, 100).1);
        assert_eq!(into[&None].bookings, 1);

        let mut huge = Tallies::from([(
            None,
            Tally {
                bookings: 1,
                sum: Decimal::MAX,
            },
        )]);
        let more = huge.clone();
        assert!(add_tallies(&mut huge, &more).is_err());
    }
}
