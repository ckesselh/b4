//! Black-box tests of the `b4` binary: what a user sees of the command line
//! itself. They run the built executable and look at exit status, stdout and
//! stderr. Only the tests of `b4 info` need a vault, the synthetic one in
//! `tests/fixtures/vault`; none needs a password or a database.

// The helpers below are not `#[test]` functions, so clippy.toml's exemption
// for tests does not reach them.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::process::{Command, Output, Stdio};

use clap::CommandFactory;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The words that select a command, without the leading `b4`.
type Path = Vec<String>;

/// A `b4` invocation that does not see the environment of the test run and
/// has no terminal on any stream.
fn b4(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_b4"));
    command
        .args(args)
        .env_remove("B4_DB")
        .env_remove("B4_PASSWORD")
        .env_remove("B4_ASKPASS")
        .env_remove("SSH_ASKPASS")
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .stdin(Stdio::null());
    command
}

fn run(args: &[&str]) -> Output {
    b4(args).output().expect("cannot run b4")
}

fn run_path(path: &Path, more: &[&str]) -> Output {
    let args: Vec<&str> = path
        .iter()
        .map(String::as_str)
        .chain(more.iter().copied())
        .collect();
    run(&args)
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is not UTF-8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr is not UTF-8")
}

fn exit_code(output: &Output) -> i32 {
    output.status.code().expect("b4 was killed by a signal")
}

/// Every command path, taken from the clap definition itself so that a new
/// command cannot be forgotten: `[]` for `b4`, `["bookings"]`,
/// `["bookings", "list"]` and so on. clap's `help` command is left out.
fn command_paths() -> Vec<Path> {
    fn walk(command: &clap::Command, path: &Path, paths: &mut Vec<Path>) {
        paths.push(path.clone());
        for subcommand in command.get_subcommands().filter(|c| c.get_name() != "help") {
            let mut longer = path.clone();
            longer.push(subcommand.get_name().to_owned());
            walk(subcommand, &longer, paths);
        }
    }

    let mut root = b4::Cli::command();
    root.build();
    let mut paths = Vec::new();
    walk(&root, &Vec::new(), &mut paths);
    assert!(
        paths.len() >= 23,
        "the walk found only {} commands",
        paths.len()
    );
    paths
}

/// The command paths that run something, as opposed to grouping commands.
fn leaf_paths() -> Vec<Path> {
    let all = command_paths();
    all.iter()
        .filter(|path| {
            !all.iter()
                .any(|other| other.len() > path.len() && other.starts_with(path))
        })
        .cloned()
        .collect()
}

/// The arguments a command cannot run without.
fn required_arguments(path: &Path) -> &'static [&'static str] {
    let words: Vec<&str> = path.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["decrypt"] | ["info"] => &["vault.sub"],
        ["accounts", "show"] => &["giro"],
        ["bookings", "show"] => &["1"],
        ["bookings", "summary"] => &["--by", "month"],
        ["sql"] => &["select 1"],
        ["export"] => &["out"],
        ["completions"] => &["bash"],
        _ => &[],
    }
}

fn is(path: &Path, words: &[&str]) -> bool {
    path.iter().map(String::as_str).eq(words.iter().copied())
}

fn display(path: &Path) -> String {
    std::iter::once("b4")
        .chain(path.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

// --- help and version ---

#[test]
fn help_works_for_every_command_path() {
    for path in command_paths() {
        let name = display(&path);
        for flag in ["--help", "-h"] {
            let output = run_path(&path, &[flag]);
            assert_eq!(exit_code(&output), 0, "{name} {flag}");
            assert_eq!(stderr(&output), "", "{name} {flag}");
            let help = stdout(&output);
            assert!(
                help.contains(&format!("Usage: {name}")),
                "{name} {flag}:\n{help}"
            );
            assert!(
                !help.contains('\x1b'),
                "{name} {flag} prints escape sequences"
            );
        }

        // `b4 help <command>` is the same as `b4 <command> --help`.
        let mut words = vec!["help"];
        words.extend(path.iter().map(String::as_str));
        let via_help = run(&words);
        assert_eq!(exit_code(&via_help), 0, "b4 help {name}");
        assert_eq!(
            stdout(&via_help),
            stdout(&run_path(&path, &["--help"])),
            "b4 help {name}"
        );
    }
}

#[test]
fn long_help_has_examples_for_every_command_path() {
    for path in command_paths() {
        let help = stdout(&run_path(&path, &["--help"]));
        let examples = help.split("\nExamples:\n").nth(1);
        let examples = examples.unwrap_or_else(|| panic!("{} has no examples", display(&path)));
        assert!(
            examples.starts_with("  b4 "),
            "{}:\n{examples}",
            display(&path)
        );
        // The short help stays short.
        let short = stdout(&run_path(&path, &["-h"]));
        assert!(!short.contains("Examples:"), "{} -h", display(&path));
    }
}

#[test]
fn the_top_level_help_explains_the_model() {
    let help = stdout(&run(&["--help"]));
    // The paragraphs are wrapped to the width of the terminal.
    let unwrapped = help.split_whitespace().collect::<Vec<_>>().join(" ");
    for needle in [
        "asks for its password once and writes the data into a DuckDB database",
        "Every other command reads only that database and never needs the password",
        "To \"lock\" the data again, delete the database file",
        "to refresh it, run `b4 decrypt` again",
        "A vault is only ever read, never changed",
    ] {
        assert!(unwrapped.contains(needle), "missing {needle:?} in:\n{help}");
    }
    for command in [
        "decrypt",
        "info",
        "status",
        "accounts",
        "bookings",
        "payees",
        "categories",
        "standing-orders",
        "portfolio",
        "sql",
        "schema",
        "export",
        "completions",
    ] {
        assert!(
            help.contains(&format!("\n  {command} ")),
            "{command} is not listed"
        );
    }
    for option in [
        "--db <PATH>",
        "-o, --output <FORMAT>",
        "--no-header",
        "-q, --quiet",
    ] {
        assert!(help.contains(option), "{option} is not listed");
    }
    assert!(help.contains("B4_DB"));
}

#[test]
fn version_works_for_every_command_path() {
    for path in command_paths() {
        let name = display(&path);
        for flag in ["--version", "-V"] {
            let output = run_path(&path, &[flag]);
            assert_eq!(exit_code(&output), 0, "{name} {flag}");
            assert_eq!(stderr(&output), "", "{name} {flag}");
            // clap names the command with hyphens: "b4-bookings-list 0.1.0".
            let expected = format!("{} {VERSION}\n", name.replace(' ', "-"));
            assert_eq!(stdout(&output), expected, "{name} {flag}");
        }
    }
}

#[test]
fn global_options_are_accepted_by_every_command() {
    for path in command_paths() {
        let help = stdout(&run_path(&path, &["-h"]));
        for option in ["--db <PATH>", "--output <FORMAT>", "--no-header", "--quiet"] {
            assert!(help.contains(option), "{} lacks {option}", display(&path));
        }
    }
}

#[test]
fn the_database_can_come_from_the_environment() {
    let output = b4(&["status", "--help"])
        .env("B4_DB", "/somewhere/else.duckdb")
        .output()
        .unwrap();
    assert!(stdout(&output).contains("[env: B4_DB=/somewhere/else.duckdb]"));
    assert!(stdout(&output).contains("[default: ./b4.duckdb]"));

    // An empty variable is a mistake, not the default.
    let output = b4(&["status"]).env("B4_DB", "").output().unwrap();
    assert_eq!(exit_code(&output), 2);
    assert!(
        stderr(&output).contains("the path is empty"),
        "{}",
        stderr(&output)
    );
}

// --- usage errors ---

#[test]
fn usage_errors_exit_with_2() {
    let cases: &[&[&str]] = &[
        // No command, or a group without a verb.
        &[],
        &["bookings"],
        &["accounts"],
        // Unknown things.
        &["frobnicate"],
        &["bookings", "delete"],
        &["status", "--frobnicate"],
        &["accounts", "list", "extra"],
        // Missing arguments.
        &["info"],
        &["accounts", "show"],
        &["bookings", "show"],
        &["bookings", "summary"],
        &["sql"],
        &["export"],
        &["completions"],
        &["bookings", "list", "--from"],
        // Values that do not parse.
        &["status", "--output", "xml"],
        &["status", "--db", ""],
        &["accounts", "list", "--type", "savings"],
        &["bookings", "list", "--from", "14.03.2026"],
        &["bookings", "list", "--to", "2026-02-30"],
        &["bookings", "list", "--min", "1.234"],
        &["bookings", "list", "--max", "1,234.56"],
        &["bookings", "list", "--min=-5"],
        &["bookings", "list", "--limit", "0"],
        &["bookings", "list", "--sort", "iban"],
        &["bookings", "show", "abc"],
        &["bookings", "summary", "--by", "week"],
        &["export", "out", "--format", "json"],
        &["completions", "powershell"],
        // Options that exclude each other.
        &["bookings", "list", "--debit", "--credit"],
        &["bookings", "list", "--pending", "--booked"],
        &["bookings", "list", "--category", "x", "--uncategorized"],
        &[
            "bookings", "summary", "--by", "month", "--debit", "--credit",
        ],
        // Ranges the wrong way round.
        &["bookings", "list", "--from", "2026-05", "--to", "2026-04"],
        &["bookings", "list", "--min", "10", "--max", "5"],
        &[
            "bookings", "summary", "--by", "year", "--from", "2027", "--to", "2026",
        ],
    ];
    for args in cases {
        let output = run(args);
        assert_eq!(exit_code(&output), 2, "{args:?}: {}", stderr(&output));
        assert_eq!(stdout(&output), "", "{args:?}");
        let message = stderr(&output);
        // Without a command clap prints the help instead of an error.
        let is_help = message.contains("Usage: b4") && message.contains("Commands:");
        assert!(
            message.starts_with("error: ") || is_help,
            "{args:?}:\n{message}"
        );
        assert!(
            !message.contains('\x1b'),
            "{args:?} prints escape sequences"
        );
    }
}

#[test]
fn usage_errors_say_what_is_wrong() {
    let message = |args: &[&str]| stderr(&run(args));

    let text = message(&["bookings", "list", "--from", "14.03.2026"]);
    assert!(
        text.contains("expected YYYY-MM-DD, YYYY-MM or YYYY"),
        "{text}"
    );

    let text = message(&["bookings", "list", "--to", "2026-02-30"]);
    assert!(text.contains("2026-02 has no day 30"), "{text}");

    let text = message(&["bookings", "list", "--min", "1.234"]);
    assert!(text.contains("ambiguous"), "{text}");

    let text = message(&["bookings", "list", "--max", "1.234,56"]);
    assert!(
        text.contains("thousands separators are not accepted"),
        "{text}"
    );

    let text = message(&["bookings", "list", "--debit", "--credit"]);
    assert!(
        text.contains("'--debit' cannot be used with '--credit'"),
        "{text}"
    );

    let text = message(&["bookings", "list", "--pending", "--booked"]);
    assert!(
        text.contains("'--pending' cannot be used with '--booked'"),
        "{text}"
    );

    let text = message(&["bookings", "list", "--from", "2026-05", "--to", "2026-04"]);
    assert_eq!(
        text,
        "error: --from (2026-05-01) is after --to (2026-04-30)\n"
    );

    let text = message(&["bookings", "list", "--min", "10", "--max", "5"]);
    assert_eq!(text, "error: --min (10) is greater than --max (5)\n");
}

// --- the password ---

#[test]
fn no_command_accepts_a_password_option() {
    for path in command_paths() {
        let name = display(&path);
        for attempt in [
            &["--password", "secret"][..],
            &["--password=secret"],
            &["--password"],
        ] {
            let mut args: Vec<&str> = required_arguments(&path).to_vec();
            args.extend_from_slice(attempt);
            let output = run_path(&path, &args);
            assert_eq!(exit_code(&output), 2, "{name} {attempt:?}");
            let message = stderr(&output);
            assert!(
                message.contains("unexpected argument '--password"),
                "{name}:\n{message}"
            );
            // Whatever was typed after the option is not echoed back.
            assert!(!message.contains("secret"), "{name}:\n{message}");
        }

        // The help of no command offers one. `decrypt` has the switch that
        // reads standard input, and that is all.
        let help = stdout(&run_path(&path, &["--help"]));
        let offered: Vec<&str> = help
            .match_indices("--password")
            .map(|(at, _)| &help[at..])
            .collect();
        for option in offered {
            assert!(
                option.starts_with("--password-stdin"),
                "{name} offers {:.30}",
                option
            );
            assert!(is(&path, &["decrypt"]), "{name} mentions --password-stdin");
        }
    }
}

#[test]
fn the_password_switch_takes_no_value() {
    // If `--password-stdin` took a value, the next word would be the
    // password. It must stay a positional argument: here, one too many.
    let output = run(&[
        "decrypt",
        "--password-stdin",
        "secret",
        "vault.sub",
        "out.duckdb",
    ]);
    assert_eq!(exit_code(&output), 2);
    let output = run(&["decrypt", "--password-stdin=secret", "vault.sub"]);
    assert_eq!(exit_code(&output), 2);
}

// --- runtime errors ---

/// The commands that never open a database.
const WITHOUT_DATABASE: &[&[&str]] = &[&["decrypt"], &["info"], &["completions"]];

#[test]
fn a_runtime_error_exits_with_1_and_a_plain_error_prefix() {
    // Every command that reads the database, given one that does not exist:
    // exit status 1, `error: ` in front, what to do about it, and no colour.
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-such-database.duckdb");
    let missing = missing.to_str().unwrap();
    let mut checked = 0;
    for path in leaf_paths() {
        if WITHOUT_DATABASE.iter().any(|words| is(&path, words)) {
            continue;
        }
        let name = display(&path);
        let mut args: Vec<&str> = path.iter().map(String::as_str).collect();
        args.extend(required_arguments(&path));
        args.extend(["--db", missing]);
        let output = b4(&args)
            .env("CLICOLOR_FORCE", "1")
            .env("TERM", "xterm-256color")
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert_eq!(exit_code(&output), 1, "{name}");
        assert_eq!(stdout(&output), "", "{name}");
        let message = stderr(&output);
        assert!(
            message.starts_with(&format!("error: there is no database at {missing}: ")),
            "{name}: {message}"
        );
        assert!(message.contains("b4 decrypt"), "{name}: {message}");
        assert!(!message.contains('\x1b'), "{name}: {message:?}");
        assert_eq!(message.lines().count(), 1, "{name}: {message}");
        checked += 1;
    }
    assert_eq!(checked, leaf_paths().len() - WITHOUT_DATABASE.len());
    assert!(checked >= 12, "{checked}");
}

#[test]
fn errors_are_not_coloured_without_a_terminal() {
    // Not even when the environment asks clap for colour: stderr is a pipe.
    for args in [
        &["status"][..],
        &["status", "--frobnicate"],
        &["bookings", "list", "--min", "x"],
    ] {
        let output = b4(args)
            .env("CLICOLOR_FORCE", "1")
            .env("TERM", "xterm-256color")
            .output();
        let output = output.unwrap();
        let message = stderr(&output);
        assert!(message.starts_with("error: "), "{args:?}:\n{message}");
        assert!(
            !message.contains('\x1b'),
            "{args:?} prints escape sequences:\n{message:?}"
        );
    }
}

// --- info ---

const DEMO_VAULT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/vault/demo.sub");

/// The `vault` object of the demo vault's manifest.
fn demo_manifest() -> serde_json::Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/vault/MANIFEST.json"
    );
    let text = std::fs::read_to_string(path).expect("cannot read the manifest");
    let manifest: serde_json::Value = serde_json::from_str(&text).expect("manifest is not JSON");
    manifest["vault"].clone()
}

/// Whether `text` has the form `2026-10-11T01:35:29Z`.
fn is_utc_timestamp(text: &str) -> bool {
    text.len() == 20
        && text
            .bytes()
            .zip("dddd-dd-ddTdd:dd:ddZ".bytes())
            .all(|(byte, shape)| {
                if shape == b'd' {
                    byte.is_ascii_digit()
                } else {
                    byte == shape
                }
            })
}

#[test]
fn info_shows_the_header_of_the_demo_vault_as_a_table() {
    let vault = demo_manifest();
    let output = run(&["info", DEMO_VAULT]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    // No password was asked for and nothing else is said.
    assert_eq!(stderr(&output), "");

    let text = stdout(&output);
    let fields: Vec<(&str, &str)> = text
        .lines()
        .map(|line| {
            let (title, value) = line.split_once(':').expect("a line without a title");
            (title, value.trim())
        })
        .collect();
    let titles: Vec<&str> = fields.iter().map(|(title, _)| *title).collect();
    assert_eq!(
        titles,
        [
            "Format version",
            "Cipher",
            "Key-stretch rounds",
            "Cluster size",
            "Vault ID",
            "Type ID",
            "Banking4 data vault",
            "Device-bound",
            "Created (UTC)",
            "Modified (UTC)",
            "File size (bytes)",
        ]
    );
    let value = |title: &str| {
        let (_, value) = fields.iter().find(|(t, _)| *t == title).unwrap();
        *value
    };
    assert_eq!(value("Format version"), "1");
    assert_eq!(value("Cipher"), "AES-256");
    assert_eq!(value("Key-stretch rounds"), "100000");
    assert_eq!(value("Cluster size"), "512");
    assert_eq!(value("Vault ID"), vault["not_reproducible"]["file_guid"]);
    assert_eq!(value("Type ID"), "6ab4f284-82a0-46fd-83f4-438632912ba7");
    assert_eq!(value("Type ID"), vault["header"]["type_guid"]);
    assert_eq!(value("Banking4 data vault"), "true");
    // The flags value 2 of the demo vault is not the device bit.
    assert_eq!(value("Device-bound"), "false");
    assert!(is_utc_timestamp(value("Created (UTC)")), "{text}");
    assert!(is_utc_timestamp(value("Modified (UTC)")), "{text}");
    assert_eq!(
        value("File size (bytes)"),
        vault["not_reproducible"]["file_size"].to_string()
    );
    assert_eq!(
        value("File size (bytes)"),
        std::fs::metadata(DEMO_VAULT).unwrap().len().to_string()
    );
}

#[test]
fn info_has_stable_keys_in_the_machine_formats() {
    const KEYS: [&str; 11] = [
        "format_version",
        "cipher",
        "kdf_rounds",
        "cluster_size",
        "vault_id",
        "type_id",
        "banking4_vault",
        "device_bound",
        "created_at",
        "modified_at",
        "file_size",
    ];
    let vault = demo_manifest();

    let output = run(&["info", DEMO_VAULT, "--output", "json"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    assert_eq!(stderr(&output), "");
    let json: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    let object = json.as_object().expect("one object");
    let keys: Vec<&str> = KEYS
        .iter()
        .copied()
        .filter(|key| object.contains_key(*key))
        .collect();
    assert_eq!(keys, KEYS);
    assert_eq!(object.len(), KEYS.len());
    // The keys come in this order in the text, too.
    let text = stdout(&output);
    let positions: Vec<usize> = KEYS
        .iter()
        .map(|key| text.find(&format!("\"{key}\":")).unwrap())
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));

    assert_eq!(json["format_version"], 1);
    assert_eq!(json["cipher"], "AES-256");
    assert_eq!(json["kdf_rounds"], 100_000);
    assert_eq!(json["kdf_rounds"], vault["header"]["kdf_rounds"]);
    assert_eq!(json["cluster_size"], 512);
    assert_eq!(json["cluster_size"], vault["header"]["cluster_size"]);
    assert_eq!(json["vault_id"], vault["not_reproducible"]["file_guid"]);
    assert_eq!(json["type_id"], vault["header"]["type_guid"]);
    assert_eq!(json["banking4_vault"], true);
    assert_eq!(json["device_bound"], false);
    assert!(is_utc_timestamp(json["created_at"].as_str().unwrap()));
    assert!(is_utc_timestamp(json["modified_at"].as_str().unwrap()));
    assert_eq!(json["file_size"], vault["not_reproducible"]["file_size"]);

    // jsonl is the same object on one line.
    let output = run(&["info", DEMO_VAULT, "-o", "jsonl"]);
    assert_eq!(exit_code(&output), 0);
    let line = stdout(&output);
    assert_eq!(line.lines().count(), 1);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap(),
        json
    );

    // csv: the keys as the header, one row; --no-header leaves the row.
    let output = run(&["info", DEMO_VAULT, "-o", "csv"]);
    assert_eq!(exit_code(&output), 0);
    let csv = stdout(&output);
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines.len(), 2, "{csv}");
    assert_eq!(lines[0], KEYS.join(","));
    assert!(lines[1].starts_with("1,AES-256,100000,512,"), "{csv}");
    assert_eq!(lines[1].split(',').count(), KEYS.len());
    let output = run(&["info", DEMO_VAULT, "-o", "csv", "--no-header"]);
    assert_eq!(stdout(&output), format!("{}\n", lines[1]));
}

#[test]
fn info_refuses_what_is_not_a_vault() {
    let dir = tempfile::tempdir().unwrap();
    let demo = std::fs::read(DEMO_VAULT).unwrap();
    let failure = |path: &std::path::Path| {
        let output = run(&["info", path.to_str().unwrap()]);
        assert_eq!(exit_code(&output), 1, "{}", path.display());
        assert_eq!(stdout(&output), "", "{}", path.display());
        stderr(&output)
    };
    const NOT_A_VAULT: &str =
        "error: not a Banking4 vault: the file does not start with a container header\n";

    // A file of another kind, long enough to hold a header.
    let other = dir.path().join("notes.sub");
    std::fs::write(&other, "x".repeat(4096)).unwrap();
    assert_eq!(failure(&other), NOT_A_VAULT);

    // A header that was cut off (01 §3.2), and an empty file.
    let truncated = dir.path().join("truncated.sub");
    std::fs::write(&truncated, &demo[..511]).unwrap();
    assert_eq!(failure(&truncated), NOT_A_VAULT);
    let empty = dir.path().join("empty.sub");
    std::fs::write(&empty, b"").unwrap();
    assert_eq!(failure(&empty), NOT_A_VAULT);

    // A container of a version b4 does not know.
    let newer = dir.path().join("newer.sub");
    let mut bytes = demo[..512].to_vec();
    bytes[3] = 2;
    std::fs::write(&newer, &bytes).unwrap();
    assert_eq!(
        failure(&newer),
        "error: unsupported vault format: fourth magic byte is 2, only 1 is known\n"
    );

    // A file that is not there, and a directory.
    let missing = dir.path().join("missing.sub");
    let message = failure(&missing);
    assert!(
        message.starts_with(&format!(
            "error: cannot read vault file {}: ",
            missing.display()
        )),
        "{message}"
    );
    assert_eq!(message.lines().count(), 1, "{message}");
    assert!(failure(dir.path()).starts_with("error: cannot read vault file "));

    // The header alone is enough: `info` reads nothing behind it.
    let header_only = dir.path().join("header-only.sub");
    std::fs::write(&header_only, &demo[..512]).unwrap();
    let output = run(&["info", header_only.to_str().unwrap(), "-o", "json"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    let json: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(json["file_size"], 512);
    assert_eq!(json["cipher"], "AES-256");
}

#[test]
fn info_leaves_the_vault_and_its_directory_alone() {
    // 01 §11: read access only, no lock file, no database.
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("demo.sub");
    std::fs::copy(DEMO_VAULT, &vault).unwrap();
    let before = std::fs::read(&vault).unwrap();
    let output = b4(&["info", "demo.sub"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    assert_eq!(std::fs::read(&vault).unwrap(), before);
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["demo.sub"]);
}

// --- completions ---

#[test]
fn completions_print_a_script_for_each_shell() {
    for (shell, marker) in [
        ("bash", "complete -F _b4"),
        ("zsh", "#compdef b4"),
        ("fish", "complete -c b4"),
    ] {
        let output = run(&["completions", shell]);
        assert_eq!(exit_code(&output), 0, "{shell}");
        assert_eq!(stderr(&output), "", "{shell}");
        let script = stdout(&output);
        assert!(script.contains(marker), "{shell}: no {marker:?}");
        // The script knows the commands and options of this binary.
        for word in [
            "bookings",
            "standing-orders",
            "summary",
            "password-stdin",
            "no-header",
        ] {
            assert!(script.contains(word), "{shell}: {word} is missing");
        }
    }
}

#[test]
fn completions_do_not_depend_on_the_other_options() {
    let plain = stdout(&run(&["completions", "bash"]));
    let with_options = stdout(&run(&[
        "completions",
        "bash",
        "-o",
        "json",
        "--db",
        "x",
        "-q",
    ]));
    assert_eq!(plain, with_options);
}

// --- closed pipes ---

/// `b4 ... | head` must end quietly when the reader has seen enough. The
/// shell closes the read end of the pipe before b4 starts writing.
#[cfg(unix)]
#[test]
fn a_closed_pipe_ends_quietly() {
    let dir = tempfile::tempdir().unwrap();
    let errors = dir.path().join("stderr");
    let status = dir.path().join("status");
    let script = r#"{ sleep 0.3; "$B4" completions zsh 2>"$ERRORS"; echo $? >"$STATUS"; } | true"#;
    let shell = Command::new("sh")
        .args(["-c", script])
        .env("B4", env!("CARGO_BIN_EXE_b4"))
        .env("ERRORS", &errors)
        .env("STATUS", &status)
        .status()
        .expect("cannot run sh");
    assert!(shell.success());

    assert_eq!(
        std::fs::read_to_string(&errors).unwrap(),
        "",
        "b4 complained"
    );
    assert_eq!(
        std::fs::read_to_string(&status).unwrap().trim(),
        "0",
        "b4 failed"
    );
}
