use std::{fmt, ops::Deref};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Error {
    pub msg: String,
    pub help: Option<String>,
}

impl Error {
    pub fn with_help(msg: impl Into<String>, help: impl Into<String>) -> Error {
        Error {
            msg: msg.into(),
            help: Some(help.into()),
        }
    }
}

impl From<String> for Error {
    fn from(msg: String) -> Error {
        Error { msg, help: None }
    }
}

impl From<&str> for Error {
    fn from(msg: &str) -> Error {
        Error::from(msg.to_string())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for Error {}

impl Deref for Error {
    type Target = str;

    fn deref(&self) -> &str {
        &self.msg
    }
}

impl PartialEq<&str> for Error {
    fn eq(&self, other: &&str) -> bool {
        self.msg == *other && self.help.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_string_has_no_help() {
        let err: Error = format!("no {}", "manifest").into();
        assert_eq!(err.msg, "no manifest");
        assert_eq!(err.help, None);
        assert_eq!(err, "no manifest");
    }

    #[test]
    fn with_help_keeps_hint_out_of_message() {
        let err = Error::with_help("nukleus not found", "put it on PATH");
        assert_eq!(err.to_string(), "nukleus not found");
        assert_eq!(err.help.as_deref(), Some("put it on PATH"));
        assert!(err.contains("found"));
        assert_ne!(err, "nukleus not found");
    }
}
