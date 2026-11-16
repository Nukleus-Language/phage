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
            let release = release_flag(&args[2..], "build").unwrap_or_else(|e| fail(&e));
            build::build(release).unwrap_or_else(|e| fail(&e));
        }
        "check" => {
            if let Some(other) = args.get(2) {
                fail(&format!("Unknown option '{}' for 'check'.", other));
            }
            build::check().unwrap_or_else(|e| fail(&e));
        }
        "run" => {
            let (flags, program_args) = match args[2..].iter().position(|a| a == "--") {
                Some(i) => (&args[2..2 + i], &args[3 + i..]),
                None => (&args[2..], &args[args.len()..]),
            };
            let release = release_flag(flags, "run").unwrap_or_else(|e| fail(&e));
            let code = build::run(release, program_args).unwrap_or_else(|e| fail(&e));
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

fn release_flag(args: &[String], command: &str) -> Result<bool, String> {
    let mut release = false;
    for arg in args {
        match arg.as_str() {
            "--release" => release = true,
            other => {
                return Err(format!("Unknown option '{}' for '{}'.", other, command));
            }
        }
    }
    Ok(release)
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
        "  {}              Build the project (--release for an optimized build)",
        ColoredString::new("build", h3_style)
    );
    println!(
        "  {}                Build and run the project (args after -- go to the program)",
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
    fn scaffold_args_rejects_second_name() {
        let err = scaffold_args(&args(&["a", "b"]), "new").unwrap_err();
        assert_eq!(err, "Unexpected argument 'b' for 'new'.");
    }
}
