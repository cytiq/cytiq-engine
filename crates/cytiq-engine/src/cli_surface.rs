//! The command-line contract as data: every (sub)command's arguments and the
//! configuration keys each command records in provenance.
//!
//! `cli_surface_matches_the_reviewed_contract` compares this with
//! `tests/fixtures/cli-surface.json`. A difference means a flag, default,
//! value set or recorded config key changed. Review it; when intended, copy the
//! actual surface the test writes next to the temporary directory over the
//! fixture in the same change. The test never rewrites the fixture.

use std::collections::BTreeMap;

use clap::{Arg, ArgAction, Command, CommandFactory};
use serde_json::{Map, Value, json};

use crate::cli::Cli;

/// Placeholder used for every required value when building a minimal
/// invocation; parses as a path, a number and a channel list.
const PLACEHOLDER: &str = "1";

fn action_name(action: &ArgAction) -> &'static str {
    match action {
        ArgAction::Set => "set",
        ArgAction::Append => "append",
        ArgAction::SetTrue => "set_true",
        ArgAction::SetFalse => "set_false",
        ArgAction::Count => "count",
        ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong => "help",
        ArgAction::Version => "version",
        _ => "other",
    }
}

fn describe_arg(command: &Command, arg: &Arg) -> Value {
    let mut conflicts = command
        .get_arg_conflicts_with(arg)
        .iter()
        .map(|other| other.get_id().to_string())
        .collect::<Vec<_>>();
    conflicts.sort();
    let defaults = arg
        .get_default_values()
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let possible = arg
        .get_possible_values()
        .iter()
        .filter(|value| !value.is_hide_set())
        .map(|value| value.get_name().to_owned())
        .collect::<Vec<_>>();
    let mut aliases = arg.get_visible_aliases().unwrap_or_default();
    aliases.sort_unstable();
    json!({
        "id": arg.get_id().as_str(),
        "long": arg.get_long(),
        "short": arg.get_short().map(String::from),
        "aliases": aliases,
        "positional": arg.is_positional(),
        "required": arg.is_required_set(),
        "global": arg.is_global_set(),
        "hidden": arg.is_hide_set(),
        "action": action_name(arg.get_action()),
        "num_args": arg.get_num_args().map(|range| format!("{range:?}")),
        "value_names": arg.get_value_names().map(|names| names.iter().map(ToString::to_string).collect::<Vec<_>>()),
        "defaults": defaults,
        "possible_values": possible,
        "conflicts_with": conflicts,
    })
}

fn describe_command(command: &Command) -> Value {
    let args = command
        .get_arguments()
        .filter(|arg| {
            !matches!(
                arg.get_action(),
                ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong | ArgAction::Version
            )
        })
        .map(|arg| describe_arg(command, arg))
        .collect::<Vec<_>>();
    let subcommands = command
        .get_subcommands()
        .filter(|sub| sub.get_name() != "help")
        .map(|sub| (sub.get_name().to_owned(), describe_command(sub)))
        .collect::<BTreeMap<_, _>>();
    json!({
        "args": args,
        "args_conflict_with_subcommands": command.is_args_conflicts_with_subcommands_set(),
        "subcommand_required": command.is_subcommand_required_set(),
        "subcommands": subcommands,
    })
}

/// Leaf command paths (`["flowsom", "train"]`) and their minimal invocations:
/// every required argument set to [`PLACEHOLDER`] or its first possible value.
fn minimal_invocations(command: &Command, path: &[String], out: &mut Vec<(String, Vec<String>)>) {
    let subcommands = command
        .get_subcommands()
        .filter(|sub| sub.get_name() != "help")
        .collect::<Vec<_>>();
    let leaf_args = |command: &Command| {
        let mut argv = Vec::new();
        for arg in command.get_arguments().filter(|arg| arg.is_required_set()) {
            let value = arg.get_possible_values().first().map_or_else(
                || PLACEHOLDER.to_owned(),
                |value| value.get_name().to_owned(),
            );
            if let Some(long) = arg.get_long() {
                argv.push(format!("--{long}"));
            }
            argv.push(value);
        }
        argv
    };
    if !command.is_subcommand_required_set() && !path.is_empty() {
        let mut argv = path.to_vec();
        argv.extend(leaf_args(command));
        out.push((path.join(" "), argv));
    }
    for sub in subcommands {
        let mut sub_path = path.to_vec();
        sub_path.push(sub.get_name().to_owned());
        minimal_invocations(sub, &sub_path, out);
    }
}

/// Dotted key paths of a JSON value; array elements share their parent's path.
fn key_paths(value: &Value, prefix: &str, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                out.push(path.clone());
                key_paths(child, &path, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                key_paths(item, prefix, out);
            }
        }
        _ => {}
    }
}

/// Provenance `config` keys recorded by each leaf command.
fn config_keys() -> BTreeMap<String, Vec<String>> {
    let mut invocations = Vec::new();
    minimal_invocations(&Cli::command(), &[], &mut invocations);
    invocations
        .into_iter()
        .map(|(name, argv)| {
            let full = std::iter::once("cytiq".to_owned()).chain(argv.iter().cloned());
            let cli = <Cli as clap::Parser>::try_parse_from(full).unwrap_or_else(|error| {
                panic!("minimal `{name}` invocation {argv:?} must parse: {error}")
            });
            let config = serde_json::to_value(&cli.command).expect("commands serialize");
            let mut keys = Vec::new();
            key_paths(&config, "", &mut keys);
            keys.sort();
            keys.dedup();
            (name, keys)
        })
        .collect()
}

pub(crate) fn surface() -> Value {
    let mut map = Map::new();
    map.insert("command".to_owned(), describe_command(&Cli::command()));
    map.insert(
        "config_keys".to_owned(),
        serde_json::to_value(config_keys()).expect("config keys serialize"),
    );
    Value::Object(map)
}

#[test]
fn cli_surface_matches_the_reviewed_contract() {
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cli-surface.json");
    let mut actual = serde_json::to_string_pretty(&surface()).expect("surface serializes");
    actual.push('\n');
    let expected = std::fs::read_to_string(&fixture).unwrap_or_default();
    if actual != expected {
        let written = std::env::temp_dir().join("cytiq-cli-surface.actual.json");
        std::fs::write(&written, &actual).expect("actual surface is written");
        panic!(
            "the CLI surface differs from {}; review `diff {} {}` and update the fixture \
             only for an intended flag or config change",
            fixture.display(),
            fixture.display(),
            written.display()
        );
    }
}
