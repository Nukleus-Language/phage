use crate::{
    build, help, project,
    ui::{self, ColorChoice, Ui, Verbosity},
};
use std::{
    env,
    io::{self, IsTerminal},
    process,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Globals {
    color: ColorChoice,
    verbosity: Verbosity,
}

pub fn fail(color: bool, msg: &str) -> ! {
    ui::print_error(msg, color);
    process::exit(1);
}

pub fn run(args: &[String]) {
    let no_color = env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
    let stderr_tty = io::stderr().is_terminal();
    let argv = args.get(1..).unwrap_or_default();
    let (globals, rest) = split_globals(argv).unwrap_or_else(|e| {
        fail(
            ui::color_enabled(ColorChoice::Auto, no_color, stderr_tty),
            &e,
        )
    });
    let ui = Ui {
        color: ui::color_enabled(globals.color, no_color, stderr_tty),
        verbosity: globals.verbosity,
    };
    let stdout_color = ui::color_enabled(globals.color, no_color, io::stdout().is_terminal());
    let code = dispatch(&rest, ui, stdout_color).unwrap_or_else(|e| fail(ui.color, &e));
    process::exit(code);
}

fn split_globals(args: &[String]) -> Result<(Globals, Vec<String>), String> {
    let mut color = ColorChoice::default();
    let mut quiet = false;
    let mut verbose = false;
    let mut rest = Vec::with_capacity(args.len());
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--" => {
                rest.push(arg.clone());
                rest.extend(iter.cloned());
                break;
            }
            "-q" | "--quiet" => quiet = true,
            "-v" | "--verbose" => verbose = true,
            "--color" => {
                let value = iter.next().ok_or_else(|| {
                    ui::with_help("'--color' needs a value", "expected never, always or auto")
                })?;
                color = ColorChoice::parse(value)?;
            }
            other => match other.strip_prefix("--color=") {
                Some(value) => color = ColorChoice::parse(value)?,
                None => rest.push(arg.clone()),
            },
        }
    }
    if quiet && verbose {
        return Err("'--verbose' and '--quiet' cannot be used together.".to_string());
    }
    let verbosity = match (quiet, verbose) {
        (true, _) => Verbosity::Quiet,
        (_, true) => Verbosity::Verbose,
        _ => Verbosity::Normal,
    };
    Ok((Globals { color, verbosity }, rest))
}

fn dispatch(rest: &[String], ui: Ui, stdout_color: bool) -> Result<i32, String> {
    let Some((name, args)) = rest.split_first() else {
        print!("{}", help::main_help(stdout_color));
        return Ok(1);
    };
    match name.as_str() {
        "-V" | "--version" => {
            println!("Phage version 0.1.0");
            Ok(0)
        }
        "-h" | "--help" => {
            print!("{}", help::main_help(stdout_color));
            Ok(0)
        }
        "help" => {
            match args.first() {
                None => print!("{}", help::main_help(stdout_color)),
                Some(topic) => {
                    let command = help::find(topic).ok_or_else(|| unknown_command(topic))?;
                    print!("{}", help::command_help(command, stdout_color));
                }
            }
            Ok(0)
        }
        name => {
            let command = help::find(name).ok_or_else(|| unknown_command(name))?;
            if wants_help(args) {
                print!("{}", help::command_help(command, stdout_color));
                return Ok(0);
            }
            match name {
                "init" => init(args),
                "new" => new(args),
                "check" => check(args, ui),
                "build" => build_cmd(args, ui),
                "test" => test(args, ui),
                "run" => run_cmd(args, ui),
                _ => Err(unknown_command(name)),
            }
        }
    }
}

fn wants_help(args: &[String]) -> bool {
    args.iter()
        .take_while(|arg| arg.as_str() != "--")
        .any(|arg| arg == "-h" || arg == "--help")
}

fn unknown_command(name: &str) -> String {
    let msg = format!("no such command: '{}'", name);
    match help::suggest(name) {
        Some(close) => ui::with_help(msg, format!("a similar command exists: '{}'", close)),
        None => ui::with_help(msg, "run 'phage help' to list the commands"),
    }
}

fn usage_error(msg: String, command: &str) -> String {
    ui::with_help(msg, format!("run 'phage help {}' for usage", command))
}

fn init(args: &[String]) -> Result<i32, String> {
    let (name, kind) = scaffold_args(args, "init").map_err(|e| usage_error(e, "init"))?;
    if name.is_some() {
        return Err("'init' takes no project name; use 'new' for a new directory.".to_string());
    }
    let path =
        env::current_dir().map_err(|e| format!("Could not read the current directory: {}", e))?;
    project::init(&path, kind).map_err(|e| format!("Could not create project: {}", e))?;
    Ok(0)
}

fn new(args: &[String]) -> Result<i32, String> {
    let (name, kind) = scaffold_args(args, "new").map_err(|e| usage_error(e, "new"))?;
    let name =
        name.ok_or_else(|| usage_error("Please provide a project name.".to_string(), "new"))?;
    project::new(name, kind)?;
    Ok(0)
}

fn check(args: &[String], ui: Ui) -> Result<i32, String> {
    let opts = flags(args, "check").map_err(|e| usage_error(e, "check"))?;
    if opts.release {
        return Err(usage_error(
            "Unknown option '--release' for 'check'.".to_string(),
            "check",
        ));
    }
    build::check(opts, ui)?;
    Ok(0)
}

fn build_cmd(args: &[String], ui: Ui) -> Result<i32, String> {
    let opts = flags(args, "build").map_err(|e| usage_error(e, "build"))?;
    build::build(opts, ui)?;
    Ok(0)
}

fn test(args: &[String], ui: Ui) -> Result<i32, String> {
    let (opts, filter) = test_flags(args).map_err(|e| usage_error(e, "test"))?;
    build::test(opts, filter.as_deref(), ui)
}

fn run_cmd(args: &[String], ui: Ui) -> Result<i32, String> {
    let (opts, program_args) = match args.iter().position(|a| a == "--") {
        Some(i) => (&args[..i], &args[i + 1..]),
        None => (args, &args[args.len()..]),
    };
    let (jit, opts) = run_flags(opts).map_err(|e| usage_error(e, "run"))?;
    if jit {
        build::run_jit(opts, program_args, ui)
    } else {
        build::run(opts, program_args, ui)
    }
}

fn scaffold_args<'a>(
    args: &'a [String],
    command: &str,
) -> Result<(Option<&'a str>, project::Kind), String> {
    let mut name = None;
    let mut kind = project::Kind::Bin;
    for arg in args {
        match arg.as_str() {
            "--lib" => kind = project::Kind::Lib,
            other if other.starts_with('-') => {
                return Err(format!("Unknown option '{}' for '{}'.", other, command));
            }
            other if name.is_none() => name = Some(other),
            other => {
                return Err(format!(
                    "Unexpected argument '{}' for '{}'.",
                    other, command
                ));
            }
        }
    }
    Ok((name, kind))
}

fn flags(args: &[String], command: &str) -> Result<build::Options, String> {
    let mut flags = build::Options::default();
    for arg in args {
        match arg.as_str() {
            "--release" => flags.release = true,
            "--locked" => flags.locked = true,
            "--offline" => flags.offline = true,
            other => {
                return Err(format!("Unknown option '{}' for '{}'.", other, command));
            }
        }
    }
    Ok(flags)
}

fn run_flags(args: &[String]) -> Result<(bool, build::Options), String> {
    let jit = args.iter().any(|arg| arg == "--jit");
    let rest: Vec<String> = args.iter().filter(|arg| *arg != "--jit").cloned().collect();
    Ok((jit, flags(&rest, "run")?))
}

fn test_flags(args: &[String]) -> Result<(build::Options, Option<String>), String> {
    let mut opts = build::Options::default();
    let mut filter = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--locked" => opts.locked = true,
            "--offline" => opts.offline = true,
            "--filter" => {
                let value = rest
                    .next()
                    .ok_or_else(|| "'--filter' needs a value.".to_string())?;
                filter = Some(value.clone());
            }
            other => return Err(format!("Unknown option '{}' for 'test'.", other)),
        }
    }
    Ok((opts, filter))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn scaffold_args_takes_name_and_lib_flag_in_either_order() {
        assert_eq!(
            scaffold_args(&args(&["--lib", "demo"]), "new"),
            Ok((Some("demo"), project::Kind::Lib))
        );
        assert_eq!(
            scaffold_args(&args(&["demo"]), "new"),
            Ok((Some("demo"), project::Kind::Bin))
        );
    }

    #[test]
    fn scaffold_args_without_name_is_none() {
        assert_eq!(
            scaffold_args(&args(&[]), "init"),
            Ok((None, project::Kind::Bin))
        );
    }

    #[test]
    fn scaffold_args_rejects_unknown_option() {
        let err = scaffold_args(&args(&["--bin", "demo"]), "new").unwrap_err();
        assert_eq!(err, "Unknown option '--bin' for 'new'.");
    }

    #[test]
    fn flags_accept_offline() {
        assert_eq!(
            flags(&args(&["--offline"]), "run"),
            Ok(build::Options {
                offline: true,
                ..Default::default()
            })
        );
    }

    #[test]
    fn flags_accept_release_and_locked_together() {
        assert_eq!(
            flags(&args(&["--locked", "--release"]), "build"),
            Ok(build::Options {
                release: true,
                locked: true,
                offline: false,
            })
        );
    }

    #[test]
    fn flags_reject_unknown_option() {
        let err = flags(&args(&["--frozen"]), "build").unwrap_err();
        assert_eq!(err, "Unknown option '--frozen' for 'build'.");
    }

    #[test]
    fn test_flags_take_filter_and_reject_release() {
        let (opts, filter) = test_flags(&args(&["--filter", "geom", "--locked"])).unwrap();
        assert!(opts.locked);
        assert_eq!(filter.as_deref(), Some("geom"));
        assert_eq!(
            test_flags(&args(&["--release"])).unwrap_err(),
            "Unknown option '--release' for 'test'."
        );
        assert_eq!(
            test_flags(&args(&["--filter"])).unwrap_err(),
            "'--filter' needs a value."
        );
    }

    #[test]
    fn run_flags_take_jit_alongside_build_flags() {
        assert_eq!(
            run_flags(&args(&["--jit", "--release"])),
            Ok((
                true,
                build::Options {
                    release: true,
                    ..Default::default()
                }
            ))
        );
        assert_eq!(
            run_flags(&args(&["--locked"])),
            Ok((
                false,
                build::Options {
                    locked: true,
                    ..Default::default()
                }
            ))
        );
        assert_eq!(
            run_flags(&args(&["--frozen"])).unwrap_err(),
            "Unknown option '--frozen' for 'run'."
        );
    }

    #[test]
    fn scaffold_args_rejects_second_name() {
        let err = scaffold_args(&args(&["a", "b"]), "new").unwrap_err();
        assert_eq!(err, "Unexpected argument 'b' for 'new'.");
    }

    #[test]
    fn globals_are_taken_from_anywhere_before_program_args() {
        let (globals, rest) = split_globals(&args(&[
            "-q",
            "build",
            "--color=never",
            "--release",
            "run",
            "--",
            "-v",
            "--color",
        ]))
        .unwrap();
        assert_eq!(globals.verbosity, Verbosity::Quiet);
        assert_eq!(globals.color, ColorChoice::Never);
        assert_eq!(
            rest,
            args(&["build", "--release", "run", "--", "-v", "--color"])
        );
    }

    #[test]
    fn verbose_and_quiet_cannot_be_combined() {
        let err = split_globals(&args(&["-v", "-q", "build"])).unwrap_err();
        assert!(err.contains("cannot be used together"), "{err}");
    }

    #[test]
    fn color_flag_needs_a_known_value() {
        assert_eq!(
            split_globals(&args(&["--color", "always", "check"]))
                .unwrap()
                .0
                .color,
            ColorChoice::Always
        );
        let err = split_globals(&args(&["--color", "sometimes"])).unwrap_err();
        assert!(err.contains("sometimes"), "{err}");
        let missing = split_globals(&args(&["--color"])).unwrap_err();
        assert!(missing.contains("needs a value"), "{missing}");
    }

    #[test]
    fn help_request_stops_at_double_dash() {
        assert!(wants_help(&args(&["--release", "-h"])));
        assert!(!wants_help(&args(&["--", "--help"])));
    }

    #[test]
    fn unknown_command_suggests_closest_name() {
        let err = unknown_command("buidl");
        let (msg, hint) = ui::split_help(&err);
        assert_eq!(msg, "no such command: 'buidl'");
        assert_eq!(hint, Some("a similar command exists: 'build'"));
    }

    #[test]
    fn usage_errors_point_at_command_help() {
        let err = usage_error(flags(&args(&["--frozen"]), "build").unwrap_err(), "build");
        let (_, hint) = ui::split_help(&err);
        assert_eq!(hint, Some("run 'phage help build' for usage"));
    }
}
