//! The docs are tests.
//!
//! `console_examples` runs every `console` block in README.md and
//! docs/*.md against the built binary with trycmd, and compares the
//! printed output with the block. Each page runs top to bottom in its own
//! scratch directory, with `HOME` in that directory. `tundish` there is
//! a wrapper that adds the hidden `--db`, `--config` and `--now` flags, so
//! a page reads a fresh database, an empty config, and a fixed clock.
//! Other commands, such as `gaff`, come from `PATH`.
//!
//! A page can have fixtures in tests/docs/<page>/: `setup.md` holds
//! `console` blocks that run before the page, for examples that assume
//! earlier state; `work/` is copied into the working directory and
//! `home/` into `HOME`.
//!
//! `cli_reference_matches_help` checks docs/cli-reference.md: the text
//! between its generated markers is the `--help` output of every visible
//! command, read from the built binary.
//!
//! Write the real output into the pages, then read the diff:
//! `TRYCMD=overwrite cargo test --test docs` for the examples,
//! `SNAPSHOTS=overwrite cargo test --test docs` for the CLI reference.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_tundish");
/// 2026-01-01T00:00:00Z. Every example sees this clock.
const NOW: i64 = 1_767_225_600;
const BEGIN: &str = "<!-- Begin generated CLI help: cargo test --test docs -->";
const END: &str = "<!-- End generated CLI help -->";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// README.md and every page in docs/, in name order.
fn pages() -> Vec<PathBuf> {
    let mut pages: Vec<PathBuf> = std::fs::read_dir(root().join("docs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    pages.sort();
    pages.insert(0, root().join("README.md"));
    pages
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn console_examples() {
    let guard = tempfile::tempdir().unwrap();
    // Canonical, so a tool that prints a resolved path prints the same
    // path trycmd writes as `[CWD]`.
    let scratch = guard.path().canonicalize().unwrap();
    let wrapper = write_wrapper(&scratch);
    let mut failed = Vec::new();
    for page in pages() {
        if std::panic::catch_unwind(|| run_page(&scratch, &wrapper, &page)).is_err() {
            failed.push(page.strip_prefix(root()).unwrap().display().to_string());
        }
    }
    assert!(
        failed.is_empty(),
        "examples differ from the binary in: {failed:?}"
    );
}

/// Run one page's `console` blocks in `scratch/<stem>/`.
fn run_page(scratch: &Path, wrapper: &Path, page: &Path) {
    let stem = page.file_stem().unwrap().to_string_lossy().into_owned();
    let fixtures = root().join("tests/docs").join(&stem);
    let dir = scratch.join(&stem);
    // trycmd runs `page.md` in `page.in/`.
    let work = dir.join("page.in");
    let home = dir.join("home");
    for (from, to) in [
        (fixtures.join("work"), &work),
        (fixtures.join("home"), &home),
    ] {
        if from.is_dir() {
            copy_dir(&from, to);
        } else {
            std::fs::create_dir_all(to).unwrap();
        }
    }
    let setup = std::fs::read_to_string(fixtures.join("setup.md")).unwrap_or_default();
    let case = dir.join("page.md");
    let body = std::fs::read_to_string(page).unwrap();
    std::fs::write(&case, format!("{setup}{body}")).unwrap();

    let home_var = |sub: &str| home.join(sub).display().to_string();
    let cases = trycmd::TestCases::new();
    // trycmd skips, and does not fail, a command it cannot resolve, so
    // every command a page runs is registered here or the page fails.
    for name in commands(page, &format!("{setup}{body}")) {
        if name == "tundish" {
            cases.register_bin("tundish", wrapper);
        } else {
            let found = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|dir| dir.join(&name))
                .find(|p| p.is_file());
            let path = found.unwrap_or_else(|| {
                panic!("{} runs `{name}`, which is not on PATH", page.display())
            });
            cases.register_bin(name, path);
        }
    }
    cases
        .env("HOME", home.display().to_string())
        .env("XDG_CONFIG_HOME", home_var(".config"))
        .env("XDG_STATE_HOME", home_var(".local/state"))
        .env("XDG_DATA_HOME", home_var(".local/share"))
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .case(&case)
        .run();

    // TRYCMD=overwrite rewrote the scratch copy. Carry the page's part
    // back to the page; the setup part stays in tests/docs.
    if std::env::var_os("TRYCMD").is_some_and(|m| m == "overwrite") {
        let written = std::fs::read_to_string(&case).unwrap();
        let body = written.strip_prefix(&setup).unwrap_or_else(|| {
            panic!(
                "the setup for {} changed; run it again after updating setup.md",
                page.display()
            )
        });
        std::fs::write(page, body).unwrap();
    }
}

/// The lines inside the page's `console` blocks.
fn console_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    // The open fence: its backtick count and its info string.
    let mut fence: Option<(usize, &str)> = None;
    for line in text.lines() {
        let ticks = line.len() - line.trim_start_matches('`').len();
        if ticks >= 3 {
            let info = line[ticks..].trim();
            match fence {
                None => fence = Some((ticks, info)),
                Some((open, _)) if ticks >= open && info.is_empty() => fence = None,
                Some(_) => {}
            }
            continue;
        }
        if fence.is_some_and(|(_, info)| info.split(',').next() == Some("console")) {
            lines.push(line);
        }
    }
    lines
}

/// The command word of every `$ ` line in the page's `console` blocks.
///
/// Panics on an output line holding `\`: trycmd reads `\` in real output
/// as `/`, so such a line can never match the binary.
fn commands(page: &Path, text: &str) -> std::collections::BTreeSet<String> {
    let mut names = std::collections::BTreeSet::new();
    for line in console_lines(text) {
        if let Some(cmd) = line.strip_prefix("$ ") {
            if let Some(word) = cmd.split_whitespace().find(|w| !w.contains('=')) {
                names.insert(word.to_string());
            }
        } else {
            assert!(
                !line.contains('\\'),
                "{}: trycmd cannot match output holding a backslash; choose an example without one: {line}",
                page.display()
            );
        }
    }
    names
}

/// `tundish` in the examples: the built binary with the hidden test
/// flags. `$PWD` is the page's working directory, where trycmd runs it.
fn write_wrapper(scratch: &Path) -> PathBuf {
    let wrapper = scratch.join("tundish");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\n[ -f \"$PWD/.tundish.yml\" ] || echo 'format: 1' > \"$PWD/.tundish.yml\"\n\
             exec '{BIN}' --db \"$PWD/.tundish.db\" --config \"$PWD/.tundish.yml\" --now {NOW} \"$@\"\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    wrapper
}

/// Every visible command path, depth first: `[]`, `["publish"]`, ...
fn command_paths(cmd: &clap::Command, prefix: &[String], out: &mut Vec<Vec<String>>) {
    out.push(prefix.to_vec());
    for sub in cmd.get_subcommands() {
        if sub.is_hide_set() || sub.get_name() == "help" {
            continue;
        }
        let mut path = prefix.to_vec();
        path.push(sub.get_name().to_string());
        command_paths(sub, &path, out);
    }
}

fn help(path: &[String]) -> String {
    let out = Command::new(BIN)
        .args(path)
        .arg("--help")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "{path:?} --help failed");
    String::from_utf8(out.stdout).unwrap()
}

/// The generated part of the CLI reference: one section per command.
fn generated_reference() -> String {
    let mut paths = Vec::new();
    command_paths(&tundish::cli::command(), &[], &mut paths);
    let mut text = String::new();
    for path in paths {
        let name = std::iter::once("tundish".to_string())
            .chain(path.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ");
        text.push_str(&format!("\n## {name}\n\n```text\n{}```\n", help(&path)));
    }
    text
}

#[test]
fn cli_reference_matches_help() {
    let path = root().join("docs/cli-reference.md");
    let page = std::fs::read_to_string(&path).unwrap();
    let begin = page
        .find(BEGIN)
        .expect("docs/cli-reference.md lacks the begin marker")
        + BEGIN.len();
    let end = page
        .find(END)
        .expect("docs/cli-reference.md lacks the end marker");
    let generated = format!(
        "{}\n{}\n{}",
        &page[..begin],
        generated_reference(),
        &page[end..]
    );
    snapbox::Assert::new()
        .action_env("SNAPSHOTS")
        .eq(generated, snapbox::Data::read_from(&path, None).raw());
}

/// The harness itself: a page whose output differs from the binary fails,
/// and a page that matches passes.
#[test]
fn a_wrong_example_fails_and_a_right_one_passes() {
    if std::env::var_os("TRYCMD").is_some() {
        return; // overwrite mode would rewrite the wrong page and pass
    }
    let guard = tempfile::tempdir().unwrap();
    let scratch = guard.path().canonicalize().unwrap();
    let wrapper = write_wrapper(&scratch);
    let right = scratch.join("right.md");
    std::fs::write(&right, "```console\n$ tundish publish q 1\n1\n\n```\n").unwrap();
    run_page(&scratch, &wrapper, &right);
    let wrong = scratch.join("wrong.md");
    std::fs::write(&wrong, "```console\n$ tundish publish q 1\n2\n\n```\n").unwrap();
    assert!(std::panic::catch_unwind(|| run_page(&scratch, &wrapper, &wrong)).is_err());
}

#[test]
fn commands_lists_console_commands_only() {
    let page = "```sh\nls\n```\n\n````markdown\n```console\n$ inner\n```\n````\n\n```console\n$ A=1 tundish status\n\n$ gaff check\nok\n```\n";
    let names: Vec<String> = commands(Path::new("p.md"), page).into_iter().collect();
    assert_eq!(names, ["gaff", "tundish"]);
}

#[test]
#[should_panic(expected = "backslash")]
fn output_with_a_backslash_is_refused() {
    commands(
        Path::new("p.md"),
        "```console\n$ tundish show 1\n\"a\\\"b\"\n```\n",
    );
}
