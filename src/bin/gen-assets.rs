//! Build-time asset generator for b4. NOT shipped: package.nix runs this once
//! during the Nix build and then deletes the helper from $out/bin.
//!
//! It pulls the clap command straight out of the library via `Cli::command()`,
//! so the man pages and the bash/zsh/fish completions are generated from the
//! exact same definition that produces `b4 --help` and can never drift from it.
//!
//! Usage:
//!   b4-gen-assets <OUT_DIR>
//! Writes into <OUT_DIR>:
//!   * b4.1 and one page per command at every level, named after the words of
//!     the command joined with hyphens (`b4 bookings list` is
//!     b4-bookings-list.1, like git's pages);
//!   * b4.bash, _b4 and b4.fish.
//!
//! The pages are clap_mangen's, section by section, except for the end. What
//! `--help` prints after the options ("Examples:", "Environment:") clap_mangen
//! would put under one heading EXTRA as running text; here every such block
//! becomes a section of its own, set as typed, so that an example can be
//! copied out of the page. A SEE ALSO section links the pages: `man b4` lists
//! all of them.

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;

use clap::CommandFactory;
use clap_complete::{generate_to, Shell};
use clap_mangen::Man;

use b4::Cli;

const BIN_NAME: &str = "b4";

/// The manual section of all pages: user commands.
const SECTION: &str = "1";

/// What clap_mangen's sections begin with: `\*(Aq` is an apostrophe that
/// stays a plain one in every output device.
const APOSTROPHE_PREAMBLE: &str = ".ie \\n(.g .ds Aq \\(aq\n.el .ds Aq '\n";

fn main() {
    let out_dir = match env::args_os().nth(1) {
        Some(dir) => PathBuf::from(dir),
        None => {
            eprintln!("usage: {BIN_NAME}-gen-assets <OUT_DIR>");
            process::exit(2);
        }
    };

    if let Err(e) = generate(&out_dir) {
        eprintln!("{BIN_NAME}-gen-assets: {e}");
        process::exit(1);
    }
}

fn generate(out_dir: &Path) -> io::Result<()> {
    fs::create_dir_all(out_dir)?;

    // The single clap::Command, shared with `b4 --help`.
    let mut cmd = Cli::command();
    cmd.set_bin_name(BIN_NAME);

    // Man pages. clap's `help` command gets no page: it only prints the help
    // of the others. Building fills in what the pages need: the full name of
    // every command and the global options.
    let mut documented = cmd.clone().disable_help_subcommand(true);
    documented.build();
    let mut pages = Vec::new();
    collect_pages(&documented, &mut pages);
    write_pages(&documented, &[], &pages, out_dir)?;

    // Shell completions (clap_complete).
    for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
        generate_to(shell, &mut cmd, BIN_NAME, out_dir)?;
    }

    Ok(())
}

/// The page name of a command: `b4-bookings-list` for `b4 bookings list`.
fn page_name(cmd: &clap::Command) -> &str {
    cmd.get_display_name().unwrap_or_else(|| cmd.get_name())
}

/// The names of the pages of `cmd` and of all commands below it, parents
/// before their children.
fn collect_pages(cmd: &clap::Command, pages: &mut Vec<String>) {
    pages.push(page_name(cmd).to_owned());
    for subcommand in visible_subcommands(cmd) {
        collect_pages(subcommand, pages);
    }
}

fn visible_subcommands(cmd: &clap::Command) -> impl Iterator<Item = &clap::Command> {
    cmd.get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
}

/// Write the page of `cmd` and, recursively, of the commands below it.
/// `parents` are the pages above this one, the top-level page first; `all`
/// are all pages there are.
fn write_pages(
    cmd: &clap::Command,
    parents: &[String],
    all: &[String],
    out_dir: &Path,
) -> io::Result<()> {
    let name = page_name(cmd).to_owned();

    // The top-level page is the index of all pages; every other page points
    // up. The commands directly below are in SUBCOMMANDS already.
    let see_also: Vec<&str> = if parents.is_empty() {
        all.iter()
            .map(String::as_str)
            .filter(|page| *page != name)
            .collect()
    } else {
        parents.iter().map(String::as_str).collect()
    };

    let mut page = Vec::new();
    render_page(cmd, &see_also, &mut page)?;
    fs::write(
        out_dir.join(format!("{name}.{SECTION}")),
        single_preamble(&page),
    )?;

    let mut lineage = parents.to_vec();
    lineage.push(name);
    for subcommand in visible_subcommands(cmd) {
        write_pages(subcommand, &lineage, all, out_dir)?;
    }
    Ok(())
}

/// Render the page of `cmd` into `page`.
fn render_page(cmd: &clap::Command, see_also: &[&str], page: &mut Vec<u8>) -> io::Result<()> {
    let version = cmd.get_version().unwrap_or_default();
    // The footer of every page names the program, not the last word of the
    // command.
    let man = Man::new(cmd.clone())
        .section(SECTION)
        .source(format!("{BIN_NAME} {version}"));

    man.render_title(page)?;
    man.render_name_section(page)?;
    man.render_synopsis_section(page)?;
    man.render_description_section(page)?;
    if cmd.get_arguments().any(|arg| !arg.is_hide_set()) {
        man.render_options_section(page)?;
    }
    if visible_subcommands(cmd).next().is_some() {
        man.render_subcommands_section(page)?;
    }

    let after_help = cmd
        .get_after_long_help()
        .map(ToString::to_string)
        .unwrap_or_default();
    for (heading, lines) in help_blocks(&after_help) {
        writeln!(page, ".SH {}", roff_text(&heading.to_uppercase()))?;
        // No filling: one line here is one line in the page.
        writeln!(page, ".nf")?;
        for line in lines {
            writeln!(page, "{}", roff_text(line))?;
        }
        writeln!(page, ".fi")?;
    }

    if !see_also.is_empty() {
        writeln!(page, ".SH \"SEE ALSO\"")?;
        let references: Vec<String> = see_also
            .iter()
            .map(|name| format!("\\fB{}\\fR({SECTION})", roff_text(name)))
            .collect();
        writeln!(page, "{}", references.join(",\n"))?;
    }

    if cmd.get_version().is_some() {
        man.render_version_section(page)?;
    }
    Ok(())
}

/// clap_mangen starts every section it renders with the definition of the
/// string it uses for an apostrophe. Keep it once, at the top of the page.
fn single_preamble(page: &[u8]) -> String {
    let page = String::from_utf8_lossy(page);
    let sections = page.replace(APOSTROPHE_PREAMBLE, "");
    format!("{APOSTROPHE_PREAMBLE}{sections}")
}

/// Split the text `--help` prints after the options into its blocks: a
/// heading line that ends in a colon, followed by lines indented by two
/// blanks, up to an empty line. Returns each heading without the colon and
/// its lines without that indentation.
fn help_blocks(text: &str) -> Vec<(&str, Vec<&str>)> {
    let mut blocks: Vec<(&str, Vec<&str>)> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let heading = line
            .strip_suffix(':')
            .filter(|heading| !heading.starts_with(' '));
        match (heading, blocks.last_mut()) {
            (Some(heading), _) => blocks.push((heading, Vec::new())),
            (None, Some((_, lines))) => lines.push(line.strip_prefix("  ").unwrap_or(line)),
            // Text before the first heading has no section to go to.
            (None, None) => blocks.push(("Notes", vec![line])),
        }
    }
    blocks
}

/// `text` as one line of roff input that prints exactly these characters.
///
/// Besides the backslash, the characters that matter are the ones a shell
/// command is made of: roff would turn `-` into a hyphen, `'` and `` ` ``
/// into typographic quotes and `~` and `^` into accents, and the example
/// would no longer work when pasted.
fn roff_text(text: &str) -> String {
    let mut roff = String::with_capacity(text.len() + 8);
    // A dot or an apostrophe at the start of a line would be a request.
    if text.starts_with(['.', '\'']) {
        roff.push_str("\\&");
    }
    for c in text.chars() {
        match c {
            '\\' => roff.push_str("\\e"),
            '-' => roff.push_str("\\-"),
            '\'' => roff.push_str("\\(aq"),
            '`' => roff.push_str("\\(ga"),
            '~' => roff.push_str("\\(ti"),
            '^' => roff.push_str("\\(ha"),
            c => roff.push(c),
        }
    }
    roff
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pages() -> Vec<(String, String)> {
        let dir = tempfile::tempdir().unwrap();
        generate(dir.path()).unwrap();
        let mut pages: Vec<(String, String)> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == SECTION)
            })
            .map(|path| {
                let name = path.file_stem().unwrap().to_str().unwrap().to_owned();
                (name, fs::read_to_string(&path).unwrap())
            })
            .collect();
        pages.sort();
        pages
    }

    #[test]
    fn every_command_gets_a_page_named_after_its_words() {
        let names: Vec<String> = pages().into_iter().map(|(name, _)| name).collect();
        for expected in [
            "b4",
            "b4-decrypt",
            "b4-accounts",
            "b4-accounts-list",
            "b4-accounts-show",
            "b4-bookings",
            "b4-bookings-list",
            "b4-bookings-show",
            "b4-bookings-summary",
            "b4-standing-orders-list",
            "b4-completions",
        ] {
            assert!(
                names.iter().any(|name| name == expected),
                "no page {expected}"
            );
        }
        // clap's own help command has none.
        assert!(!names.iter().any(|name| name.contains("help")), "{names:?}");
    }

    #[test]
    fn the_top_level_page_lists_every_other_page() {
        let pages = pages();
        let (_, top) = pages.iter().find(|(name, _)| name == "b4").unwrap();
        let see_also = top
            .split(".SH \"SEE ALSO\"")
            .nth(1)
            .expect("b4.1 has no SEE ALSO");
        for (name, _) in pages.iter().filter(|(name, _)| name != "b4") {
            let reference = format!("\\fB{}\\fR(1)", name.replace('-', "\\-"));
            assert!(see_also.contains(&reference), "b4.1 does not list {name}");
        }
    }

    #[test]
    fn nested_pages_point_at_their_parents() {
        let pages = pages();
        let (_, page) = pages
            .iter()
            .find(|(name, _)| name == "b4-bookings-list")
            .unwrap();
        assert!(page.contains(".TH b4-bookings-list 1"));
        assert!(page.contains("\"b4 "), "the footer names the program");
        let see_also = page.split(".SH \"SEE ALSO\"").nth(1).unwrap();
        assert!(see_also.contains("\\fBb4\\fR(1)"));
        assert!(see_also.contains("\\fBb4\\-bookings\\fR(1)"));

        // The page of a group lists its commands.
        let (_, group) = pages
            .iter()
            .find(|(name, _)| name == "b4-bookings")
            .unwrap();
        let subcommands = group.split(".SH SUBCOMMANDS").nth(1).unwrap();
        assert!(subcommands.contains("b4\\-bookings\\-list(1)"));
        assert!(subcommands.contains("b4\\-bookings\\-summary(1)"));
    }

    #[test]
    fn examples_are_a_section_of_their_own() {
        for (name, page) in pages() {
            assert!(
                page.contains(".SH EXAMPLES\n.nf\n"),
                "{name} has no EXAMPLES"
            );
            assert!(!page.contains(".SH EXTRA"), "{name} has an EXTRA section");
        }
        let pages = pages();
        let (_, top) = pages.iter().find(|(name, _)| name == "b4").unwrap();
        assert!(top.contains(".SH ENVIRONMENT\n.nf\nB4_DB "));
    }

    #[test]
    fn the_apostrophe_string_is_defined_once_at_the_top() {
        for (name, page) in pages() {
            assert!(page.starts_with(APOSTROPHE_PREAMBLE), "{name}");
            assert_eq!(page.matches(".ds Aq").count(), 2, "{name}");
        }
    }

    #[test]
    fn no_page_shows_a_password_option() {
        for (name, page) in pages() {
            let without_the_switch = page.replace("\\-\\-password\\-stdin", "");
            assert!(!without_the_switch.contains("\\-\\-password"), "{name}");
        }
    }

    #[test]
    fn help_blocks_split_headings_from_lines() {
        let blocks =
            help_blocks("Examples:\n  b4 status\n      Describe it.\n\nEnvironment:\n  X  y");
        assert_eq!(
            blocks,
            vec![
                ("Examples", vec!["b4 status", "    Describe it."]),
                ("Environment", vec!["X  y"]),
            ]
        );
        assert!(help_blocks("").is_empty());
        // A colon at the end of an indented line does not start a section.
        let blocks = help_blocks("Examples:\n  b4 sql 'select 1'\n      Note this:");
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn roff_text_keeps_shell_characters_literal() {
        assert_eq!(
            roff_text("b4 sql 'select 1' --output json"),
            "b4 sql \\(aqselect 1\\(aq \\-\\-output json"
        );
        assert_eq!(
            roff_text("~/vault.sub `x` a\\b ^"),
            "\\(ti/vault.sub \\(gax\\(ga a\\eb \\(ha"
        );
        assert_eq!(roff_text(".hidden"), "\\&.hidden");
        assert_eq!(roff_text("'quoted'"), "\\&\\(aqquoted\\(aq");
        assert_eq!(roff_text("    indented"), "    indented");
    }
}
