//! Resolving an `ACCOUNT` argument to an account of the database.
//!
//! One implementation for every command that takes an account (`accounts
//! show`, `--account`). The rule is the one in the module documentation of
//! [`commands`](super): the numeric id, then the IBAN, then the account
//! number, then a part of the name, and the first rule that matches at least
//! one account decides. One refinement of the last rule: when several names
//! contain the text and exactly one of them is the text, that account is
//! meant, so an account called `Giro` can be named next to `Giro alt`.

use crate::db::read::{Database, Param};
use crate::db::schema::table;
use crate::error::Context;
use crate::output::Cell;
use crate::Error;

/// An account a reference resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Account {
    /// The id of the account (`accounts.id`).
    pub(crate) id: i64,
    /// Its name, if it has one.
    pub(crate) name: Option<String>,
}

/// The one account `reference` names.
///
/// A reference that matches several accounts is an error that lists them as
/// `id  name` lines; one that matches none is an error that says how to list
/// the accounts. Hidden accounts are found like any other.
pub(crate) fn resolve(database: &Database, reference: &str) -> Result<Account, Error> {
    let text = reference.trim();
    let mut candidates =
        candidates(database, text).context(format!("cannot look up the account `{reference}`"))?;
    match candidates.len() {
        0 => Err(Error::new(format!(
            "no account matches `{reference}`: `b4 accounts list --all` lists the accounts \
             with their ids"
        ))),
        1 => Ok(candidates.remove(0)),
        _ => {
            let mut message = format!("`{reference}` matches several accounts:");
            for account in &candidates {
                let name = account.name.as_deref().unwrap_or("");
                message.push_str(&format!("\n  {}  {name}", account.id));
            }
            message.push_str("\nname one of them by its id");
            Err(Error::new(message))
        }
    }
}

/// The ids of the accounts `references` name, each resolved with [`resolve`],
/// in the order given and without repetitions.
pub(crate) fn resolve_ids(database: &Database, references: &[String]) -> Result<Vec<i64>, Error> {
    let mut ids = Vec::with_capacity(references.len());
    for reference in references {
        let id = resolve(database, reference)?.id;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// The accounts the first matching rule finds for `text`.
fn candidates(database: &Database, text: &str) -> Result<Vec<Account>, Error> {
    // A number that is no id can still be an account number: go on then.
    if let Ok(id) = text.parse::<i64>() {
        let found = matching(database, "id = ?", Param::from(id))?;
        if !found.is_empty() {
            return Ok(found);
        }
    }

    let iban: String = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_uppercase)
        .collect();
    if !iban.is_empty() {
        let found = matching(
            database,
            "replace(upper(iban), ' ', '') = ?",
            Param::from(iban),
        )?;
        if !found.is_empty() {
            return Ok(found);
        }
    }

    let found = matching(database, "account_number = ?", Param::from(text))?;
    if !found.is_empty() {
        return Ok(found);
    }

    let mut found = matching(
        database,
        "contains(lower(name), lower(?))",
        Param::from(text),
    )?;
    if found.len() > 1 {
        let wanted = text.to_lowercase();
        let is_exact = |account: &Account| {
            account
                .name
                .as_deref()
                .is_some_and(|name| name.to_lowercase() == wanted)
        };
        if found.iter().filter(|account| is_exact(account)).count() == 1 {
            found.retain(is_exact);
        }
    }
    Ok(found)
}

/// The accounts for which `condition` holds with `param` bound to its one
/// placeholder, in the order Banking4 shows them. An id that the table holds
/// twice counts once.
fn matching(database: &Database, condition: &str, param: Param) -> Result<Vec<Account>, Error> {
    let sql = format!(
        "select id, name from {} where {condition} order by position nulls last, id, rowid",
        table::ACCOUNTS
    );
    let result = database.query(&sql, &[param])?;
    let mut accounts: Vec<Account> = Vec::new();
    for row in result.rows() {
        let Some(Cell::Integer(id)) = row.first() else {
            continue;
        };
        if accounts.iter().any(|account| account.id == *id) {
            continue;
        }
        let name = match row.get(1) {
            Some(Cell::Text(name)) => Some(name.clone()),
            _ => None,
        };
        accounts.push(Account { id: *id, name });
    }
    Ok(accounts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testdb::{self, CARD, DEPOT, GIRO, OLD};

    fn id_of(database: &Database, reference: &str) -> i64 {
        resolve(database, reference).unwrap().id
    }

    fn error_of(database: &Database, reference: &str) -> String {
        resolve(database, reference).unwrap_err().to_string()
    }

    #[test]
    fn the_numeric_id_comes_first() {
        let (_dir, database) = testdb::open();
        assert_eq!(id_of(&database, "1"), GIRO);
        assert_eq!(id_of(&database, " 3 "), OLD);
        let account = resolve(&database, "4").unwrap();
        assert_eq!(
            account,
            Account {
                id: DEPOT,
                name: Some("Depot".to_owned())
            }
        );
    }

    #[test]
    fn an_iban_matches_without_blanks_and_in_any_case() {
        let (_dir, database) = testdb::open();
        assert_eq!(id_of(&database, "DE31999900011000471100"), GIRO);
        assert_eq!(id_of(&database, "de31 9999 0001 1000 4711 00"), GIRO);
        assert_eq!(id_of(&database, "DE84 9999 0003 3000 4711 44"), OLD);
        // Part of an IBAN is not an IBAN.
        assert!(error_of(&database, "DE31").starts_with("no account matches `DE31`"));
    }

    #[test]
    fn a_number_that_is_no_id_is_tried_as_account_number() {
        let (_dir, database) = testdb::open();
        assert_eq!(id_of(&database, "4711"), CARD);
        assert_eq!(id_of(&database, "1000471100"), GIRO);
        // The id wins over the account number of another account.
        assert_eq!(id_of(&database, "2"), CARD);
    }

    #[test]
    fn a_unique_part_of_the_name_matches_in_any_case() {
        let (_dir, database) = testdb::open();
        assert_eq!(id_of(&database, "giro"), GIRO);
        assert_eq!(id_of(&database, "KREDIT"), CARD);
        assert_eq!(id_of(&database, "altes"), OLD);
        assert_eq!(id_of(&database, "depot"), DEPOT);
    }

    #[test]
    fn a_complete_name_wins_over_a_part() {
        let (_dir, database) = testdb::open_with(
            "insert into accounts (id, name, position) values (7, 'Depot alt', 9);",
        );
        assert_eq!(id_of(&database, "depot"), DEPOT);
        assert_eq!(id_of(&database, "depot alt"), 7);
        assert!(error_of(&database, "dep").contains("matches several accounts"));
    }

    #[test]
    fn an_ambiguous_reference_lists_the_candidates() {
        let (_dir, database) = testdb::open();
        assert_eq!(
            error_of(&database, "konto"),
            "`konto` matches several accounts:\n  1  Girokonto\n  3  Altes Sparkonto\n\
             name one of them by its id"
        );
    }

    #[test]
    fn an_unknown_reference_says_how_to_list_the_accounts() {
        let (_dir, database) = testdb::open();
        assert_eq!(
            error_of(&database, "nothing"),
            "no account matches `nothing`: `b4 accounts list --all` lists the accounts with \
             their ids"
        );
        // Wildcards and quotes are text, not patterns or SQL.
        for text in ["%", "_", "' or ''='", "99"] {
            assert!(
                error_of(&database, text).starts_with("no account matches"),
                "{text}"
            );
        }
    }

    #[test]
    fn a_repeated_id_is_one_account() {
        let (_dir, database) = testdb::open_with(
            "insert into accounts (id, name, position) values (1, 'Girokonto (Kopie)', 0);",
        );
        assert_eq!(id_of(&database, "1"), GIRO);
        assert_eq!(id_of(&database, "giro"), GIRO);
    }

    #[test]
    fn several_references_give_distinct_ids_in_order() {
        let (_dir, database) = testdb::open();
        let references = ["kredit", "1", "giro", "4711"].map(str::to_owned);
        assert_eq!(resolve_ids(&database, &references).unwrap(), [CARD, GIRO]);
        assert!(resolve_ids(&database, &["giro".to_owned(), "x".to_owned()]).is_err());
        assert_eq!(resolve_ids(&database, &[]).unwrap(), Vec::<i64>::new());
    }
}
