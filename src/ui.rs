use crate::error::Error;
use inksac::{Color, ColoredString, Style};
use std::{
    ffi::{OsStr, OsString},
    time::Instant,
};

const VERB_WIDTH: usize = 12;

const fn style(foreground: Color) -> Style {
    Style {
        foreground,
        background: Color::Empty,
        bold: true,
        dim: false,
        italic: false,
        underline: false,
    }
}

pub const TITLE: Style = style(Color::Cyan);
pub const HEADING: Style = style(Color::Yellow);
pub const LITERAL: Style = style(Color::Green);
const STATUS: Style = style(Color::Green);
const ERROR: Style = style(Color::Red);
const HINT: Style = style(Color::Cyan);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

impl ColorChoice {
    pub fn parse(text: &str) -> Result<Self, Error> {
        match text {
            "auto" => Ok(ColorChoice::Auto),
            "always" => Ok(ColorChoice::Always),
            "never" => Ok(ColorChoice::Never),
            other => Err(Error::with_help(
                format!("invalid value '{}' for '--color'", other),
                "expected never, always or auto",
            )),
        }
    }
}

pub fn color_enabled(choice: ColorChoice, no_color: bool, is_tty: bool) -> bool {
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => is_tty && !no_color,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Verbosity {
    Quiet,
    #[default]
    Normal,
    Verbose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ui {
    pub color: bool,
    pub verbosity: Verbosity,
}

impl Ui {
    pub fn status(&self, verb: &str, msg: &str) {
        if self.verbosity != Verbosity::Quiet {
            eprintln!("{}", status_line(verb, msg, self.color));
        }
    }

    pub fn verbose(&self, verb: &str, msg: &str) {
        if self.verbosity == Verbosity::Verbose {
            eprintln!("{}", status_line(verb, msg, self.color));
        }
    }
}

pub fn paint(text: &str, style: Style, color: bool) -> String {
    if color {
        ColoredString::new(text, style).to_string()
    } else {
        text.to_string()
    }
}

pub fn status_line(verb: &str, msg: &str, color: bool) -> String {
    let verb = format!("{:>width$}", verb, width = VERB_WIDTH);
    format!("{} {}", paint(&verb, STATUS, color), msg)
}

pub fn error_line(msg: &str, color: bool) -> String {
    format!("{} {}", paint("error:", ERROR, color), msg)
}

pub fn help_line(hint: &str, color: bool) -> String {
    format!("{} {}", paint("help:", HINT, color), hint)
}

pub fn print_error(err: &Error, color: bool) {
    eprintln!("{}", error_line(&err.msg, color));
    if let Some(hint) = &err.help {
        eprintln!("{}", help_line(hint, color));
    }
}

pub fn render_command(program: &OsStr, args: &[OsString]) -> String {
    std::iter::once(program)
        .chain(args.iter().map(OsString::as_os_str))
        .map(|part| quote(&part.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote(text: &str) -> String {
    if !text.is_empty()
        && text
            .chars()
            .all(|c| !c.is_whitespace() && c != '\'' && c != '"')
    {
        return text.to_string();
    }
    format!("'{}'", text.replace('\'', r"'\''"))
}

pub fn elapsed(start: Instant) -> String {
    format!("{:.2}s", start.elapsed().as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_line_right_aligns_verb_in_twelve_columns() {
        assert_eq!(
            status_line("Compiling", "helix v0.1.0 (/p)", false),
            "   Compiling helix v0.1.0 (/p)"
        );
        assert_eq!(
            status_line("Finished", "`dev` profile in 0.42s", false),
            "    Finished `dev` profile in 0.42s"
        );
    }

    #[test]
    fn colored_status_line_paints_only_the_verb() {
        let line = status_line("Fresh", "helix v0.1.0", true);
        assert!(line.starts_with("\x1b["), "{line:?}");
        assert!(line.ends_with(" helix v0.1.0"), "{line:?}");
        assert!(line.contains("       Fresh"), "{line:?}");
    }

    #[test]
    fn color_follows_choice_then_tty_and_no_color() {
        assert!(color_enabled(ColorChoice::Always, true, false));
        assert!(!color_enabled(ColorChoice::Never, false, true));
        assert!(color_enabled(ColorChoice::Auto, false, true));
        assert!(!color_enabled(ColorChoice::Auto, false, false));
        assert!(!color_enabled(ColorChoice::Auto, true, true));
    }

    #[test]
    fn color_choice_parses_three_values_only() {
        assert_eq!(ColorChoice::parse("never"), Ok(ColorChoice::Never));
        assert_eq!(ColorChoice::parse("always"), Ok(ColorChoice::Always));
        assert_eq!(ColorChoice::parse("auto"), Ok(ColorChoice::Auto));
        let err = ColorChoice::parse("sometimes").unwrap_err();
        assert!(err.starts_with("invalid value 'sometimes'"), "{err}");
    }

    #[test]
    fn error_help_is_printed_separately_from_message() {
        let err = Error::with_help("nukleus not found", "put it on PATH");
        assert_eq!(err.msg, "nukleus not found");
        assert_eq!(err.help.as_deref(), Some("put it on PATH"));
    }

    #[test]
    fn render_command_quotes_only_what_needs_it() {
        let rendered = render_command(
            OsStr::new("nukleus"),
            &[
                OsString::from("-o"),
                OsString::from("/p/target/debug/demo"),
                OsString::from("a b"),
                OsString::from("it's"),
            ],
        );
        assert_eq!(rendered, "nukleus -o /p/target/debug/demo 'a b' 'it'\\''s'");
    }
}
