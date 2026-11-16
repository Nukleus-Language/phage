use crate::{build, project};
use inksac::{Color, ColoredString, Style};
use std::{env, process};

const ERRORSTYLE: Style = Style {
    foreground: Color::Red,
    background: Color::Empty,
    bold: false,
    dim: false,
    italic: false,
    underline: false,
};

pub fn fail(msg: &str) -> ! {
    eprintln!("{}", ColoredString::new(msg, ERRORSTYLE));
    process::exit(1);
}

pub fn run(args: &[String]) {
    if args.len() < 2 {
        print_help_main();
        process::exit(1);
    }

    match args[1].as_str() {
        "-v" | "--version" => {
            println!("Phage version 0.1.0");
        }
        "-h" | "--help" => {
            print_help_main();
        }
        "init" => {
            let (name, kind) = scaffold_args(&args[2..], "init").unwrap_or_else(|e| fail(&e));
            if name.is_some() {
                fail("'init' takes no project name; use 'new' for a new directory.");
            }
            let path = env::current_dir()
                .unwrap_or_else(|e| fail(&format!("Could not read the current directory: {}", e)));
            project::init(&path, kind)
                .unwrap_or_else(|e| fail(&format!("Could not create project: {}", e)));
        }
        "build" => {
            let opts = flags(&args[2..], "build").unwrap_or_else(|e| fail(&e));
            build::build(opts).unwrap_or_else(|e| fail(&e));
        }
        "check" => {
            let opts = flags(&args[2..], "check").unwrap_or_else(|e| fail(&e));
            if opts.release {
                fail("Unknown option '--release' for 'check'.");
            }
            build::check(opts).unwrap_or_else(|e| fail(&e));
        }
        "test" => {
            let (opts, filter) = test_flags(&args[2..]).unwrap_or_else(|e| fail(&e));
            let code = build::test(opts, filter.as_deref()).unwrap_or_else(|e| fail(&e));
            process::exit(code);
        }
        "run" => {
            let (opts, program_args) = match args[2..].iter().position(|a| a == "--") {
                Some(i) => (&args[2..2 + i], &args[3 + i..]),
                None => (&args[2..], &args[args.len()..]),
            };
            let (jit, opts) = run_flags(opts).unwrap_or_else(|e| fail(&e));
            let result = if jit {
                build::run_jit(opts, program_args)
            } else {
                build::run(opts, program_args)
            };
            let code = result.unwrap_or_else(|e| fail(&e));
            process::exit(code);
        }
        "new" => {
            let (name, kind) = scaffold_args(&args[2..], "new").unwrap_or_else(|e| fail(&e));
            let name = name.unwrap_or_else(|| fail("Please provide a project name."));
            project::new(name, kind).unwrap_or_else(|e| fail(&e));
        }
        _ => {
            println!("{}", ColoredString::new("Invalid command.", ERRORSTYLE));
            process::exit(1);
        }
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

fn print_help_main() {
    let title_style = Style {
        foreground: Color::Cyan,
        ..Default::default()
    };
    let h2_style = Style {
        foreground: Color::Yellow,
        ..Default::default()
    };
    let h3_style = Style {
        foreground: Color::Green,
        ..Default::default()
    };
    println!(
        "{}, a package and build manager for Nukleus",
        ColoredString::new("Phage", title_style)
    );
    println!("\n{}:", ColoredString::new("Commands", h2_style));
    println!(
        "  {}               Create a new project in the current directory (--lib for a library)",
        ColoredString::new("init", h3_style)
    );
    println!(
        "  {}                Create a new project in a new directory (--lib for a library)",
        ColoredString::new("new", h3_style)
    );
    println!(
        "  {}              Type-check the project without linking",
        ColoredString::new("check", h3_style)
    );
    println!(
        "  {}              Build the project (--release for an optimized build, --locked to fail if Phage.lock would change, --offline to use only the git cache)",
        ColoredString::new("build", h3_style)
    );
    println!(
        "  {}                Build and run the tests of the project (--filter <text> runs only tests whose module.name contains text)",
        ColoredString::new("test", h3_style)
    );
    println!(
        "  {}                Build and run the project (args after -- go to the program; --jit runs it in lamina's JIT instead, x86_64 Linux only, no program args)",
        ColoredString::new("run", h3_style)
    );
    println!(
        "  {}        Print this message",
        ColoredString::new("-h, --help ", h3_style)
    );
    println!(
        "  {}      Prints the version",
        ColoredString::new("-v, --version", h3_style)
    );
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
}
