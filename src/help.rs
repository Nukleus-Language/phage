use crate::ui::{HEADING, LITERAL, TITLE, paint};

pub struct Command {
    pub name: &'static str,
    pub usage: &'static str,
    pub summary: &'static str,
    pub options: &'static [(&'static str, &'static str)],
}

const LOCKED_OFFLINE: &[(&str, &str)] = &[
    ("--locked", "Fail if Phage.lock would change"),
    ("--offline", "Use only the git cache, never the network"),
    ("-p <name>", "Only this workspace package (repeatable)"),
    (
        "--profile <name>",
        "Use this profile (dev, release, or one from Phage.toml)",
    ),
];

pub const COMMANDS: &[Command] = &[
    Command {
        name: "init",
        usage: "init [--lib]",
        summary: "Create a new project in the current directory",
        options: &[("--lib", "Make a library instead of a binary")],
    },
    Command {
        name: "new",
        usage: "new <name> [--lib]",
        summary: "Create a new project in a new directory",
        options: &[("--lib", "Make a library instead of a binary")],
    },
    Command {
        name: "check",
        usage: "check [--locked] [--offline]",
        summary: "Type-check the project without linking",
        options: LOCKED_OFFLINE,
    },
    Command {
        name: "clean",
        usage: "clean",
        summary: "Remove the target directory of the project",
        options: &[],
    },
    Command {
        name: "build",
        usage: "build [--release] [--locked] [--offline]",
        summary: "Build the project",
        options: &[
            ("--release", "Build with optimizations"),
            ("--locked", "Fail if Phage.lock would change"),
            ("--offline", "Use only the git cache, never the network"),
            ("-p <name>", "Only this workspace package (repeatable)"),
            (
                "--profile <name>",
                "Use this profile (dev, release, or one from Phage.toml)",
            ),
        ],
    },
    Command {
        name: "test",
        usage: "test [--filter <text>] [--locked] [--offline]",
        summary: "Build and run the tests of the project",
        options: &[
            (
                "--filter <text>",
                "Run only tests whose module.name contains text",
            ),
            ("--locked", "Fail if Phage.lock would change"),
            ("--offline", "Use only the git cache, never the network"),
            ("-p <name>", "Only this workspace package (repeatable)"),
            (
                "--profile <name>",
                "Use this profile (dev, release, or one from Phage.toml)",
            ),
        ],
    },
    Command {
        name: "run",
        usage: "run [--release] [--jit] [--locked] [--offline] [-- <args>]",
        summary: "Build and run the project",
        options: &[
            ("--release", "Build with optimizations"),
            ("--jit", "Run in lamina's JIT instead (x86_64 Linux only)"),
            ("--locked", "Fail if Phage.lock would change"),
            ("--offline", "Use only the git cache, never the network"),
            ("-p <name>", "Only this workspace package (repeatable)"),
            (
                "--profile <name>",
                "Use this profile (dev, release, or one from Phage.toml)",
            ),
            ("-- <args>", "Pass the remaining arguments to the program"),
        ],
    },
];

pub const GLOBAL_OPTIONS: &[(&str, &str)] = &[
    ("-q, --quiet", "Print only errors"),
    ("-v, --verbose", "Also print the full nukleus command line"),
    (
        "--color <when>",
        "never, always or auto (default; NO_COLOR turns color off)",
    ),
];

pub fn find(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|command| command.name == name)
}

pub fn suggest(name: &str) -> Option<&'static str> {
    COMMANDS
        .iter()
        .map(|command| (distance(name, command.name), command.name))
        .min()
        .filter(|(dist, _)| *dist <= 2)
        .map(|(_, name)| name)
}

pub fn main_help(color: bool) -> String {
    let mut out = format!(
        "{}, a package and build manager for Nukleus\n\n",
        paint("Phage", TITLE, color)
    );
    out += &format!(
        "{} phage [options] <command> [args]\n\n",
        paint("Usage:", HEADING, color)
    );
    out += &format!("{}\n", paint("Commands:", HEADING, color));
    let width = COMMANDS
        .iter()
        .map(|command| command.name.len())
        .max()
        .unwrap_or(0);
    for command in COMMANDS {
        out += &format!(
            "  {}  {}\n",
            paint(&pad(command.name, width), LITERAL, color),
            command.summary
        );
    }
    out += &format!("\n{}\n", paint("Options:", HEADING, color));
    out += &option_rows(GLOBAL_OPTIONS, color);
    out += &format!(
        "  {}  Print this message\n  {}  Print the version\n",
        paint(&pad("-h, --help", 15), LITERAL, color),
        paint("-V, --version", LITERAL, color)
    );
    out += "\nRun 'phage help <command>' for more information on a command.\n";
    out
}

pub fn command_help(command: &Command, color: bool) -> String {
    let mut out = format!(
        "{} phage {}\n\n{}\n\n",
        paint("Usage:", HEADING, color),
        command.usage,
        command.summary
    );
    if !command.options.is_empty() {
        out += &format!("{}\n", paint("Options:", HEADING, color));
        out += &option_rows(command.options, color);
        out += "\n";
    }
    out += &format!("{}\n", paint("Global options:", HEADING, color));
    out += &option_rows(GLOBAL_OPTIONS, color);
    out
}

fn option_rows(rows: &[(&str, &str)], color: bool) -> String {
    let width = rows.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
    rows.iter()
        .map(|(name, text)| format!("  {}  {}\n", paint(&pad(name, width), LITERAL, color), text))
        .collect()
}

fn pad(text: &str, width: usize) -> String {
    format!("{:<width$}", text, width = width)
}

fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_usage_line_starts_with_its_command_name() {
        for command in COMMANDS {
            assert!(command.usage.starts_with(command.name), "{}", command.usage);
        }
    }

    #[test]
    fn suggest_picks_the_closest_command() {
        assert_eq!(suggest("buidl"), Some("build"));
        assert_eq!(suggest("chek"), Some("check"));
        assert_eq!(suggest("zzzzzzzz"), None);
    }

    #[test]
    fn edit_distance_counts_insert_delete_and_replace() {
        assert_eq!(distance("", ""), 0);
        assert_eq!(distance("run", "run"), 0);
        assert_eq!(distance("rn", "run"), 1);
        assert_eq!(distance("tset", "test"), 2);
        assert_eq!(distance("kitten", "sitting"), 3);
    }

    #[test]
    fn command_help_shows_usage_and_its_options() {
        let text = command_help(find("build").unwrap(), false);
        assert!(text.starts_with("Usage: phage build [--release]"), "{text}");
        assert!(text.contains("--release"), "{text}");
        assert!(text.contains("--offline"), "{text}");
        assert!(text.contains("Global options:"), "{text}");
        assert!(!text.contains('\x1b'), "{text:?}");
    }

    #[test]
    fn main_help_lists_every_command() {
        let text = main_help(false);
        for command in COMMANDS {
            assert!(text.contains(command.name), "{text}");
        }
        assert!(text.contains("-V, --version"), "{text}");
    }
}
