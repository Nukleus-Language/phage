use crate::error::Error;
use crate::{manifest::GitDependency, registry::Registry, ui::Ui};
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

pub struct Store {
    home: PathBuf,
    offline: bool,
    ui: Ui,
    pub registry: Registry,
}

#[derive(Debug)]
pub struct Checkout {
    pub dir: PathBuf,
    pub sha: String,
}

impl Store {
    pub fn new(home: PathBuf, offline: bool) -> Store {
        let registry = Registry::new(home.clone(), offline, crate::registry::DEFAULT_INDEX);
        Store {
            home,
            offline,
            ui: Ui::default(),
            registry,
        }
    }

    pub fn with_ui(mut self, ui: Ui) -> Store {
        self.ui = ui;
        self.registry.ui = ui;
        self
    }

    pub fn with_index(mut self, base: &str) -> Store {
        self.registry = Registry::new(self.home.clone(), self.offline, base)
            .with_pins(std::collections::BTreeMap::new());
        self.registry.ui = self.ui;
        self
    }

    pub fn with_pins(
        mut self,
        pins: std::collections::BTreeMap<String, crate::lock::Pin>,
    ) -> Store {
        self.registry = self.registry.with_pins(pins);
        self
    }

    pub fn from_env(offline: bool) -> Result<Store, Error> {
        let home = match env::var_os("PHAGE_HOME") {
            Some(home) => PathBuf::from(home),
            None => PathBuf::from(
                env::var_os("HOME").ok_or_else(|| "HOME is not set; set PHAGE_HOME".to_string())?,
            )
            .join(".phage"),
        };
        Ok(Store::new(home, offline).with_index(&Registry::index_from_env()))
    }

    pub fn checkout(&self, name: &str, dep: &GitDependency) -> Result<Checkout, Error> {
        if dep.rev.is_some() && dep.branch.is_some() {
            return Err(Error::from(format!(
                "Dependency '{}' sets both rev and branch.",
                name
            )));
        }
        reject_option(name, "url", &dep.git)?;
        if let Some(rev) = &dep.rev {
            reject_option(name, "rev", rev)?;
        }
        if let Some(branch) = &dep.branch {
            reject_option(name, "branch", branch)?;
        }
        let key = format!("{}-{}", safe(name), short_hash(&dep.git));
        let bare = self.home.join("git").join(&key);
        self.ensure_bare(&bare, name, &dep.git)?;
        let sha = self.resolve(&bare, name, dep)?;
        let dir = self.home.join("checkouts").join(&key).join(&sha);
        if !dir.is_dir() {
            materialize(&bare, &dir, &sha)?;
        }
        Ok(Checkout { dir, sha })
    }

    fn ensure_bare(&self, bare: &Path, name: &str, url: &str) -> Result<(), Error> {
        if bare.is_dir() {
            if !self.offline {
                self.ui
                    .status("Updating", &format!("{} ({})", name, redact(url)));
                git(
                    Some(bare),
                    &[
                        "fetch",
                        "--force",
                        "--quiet",
                        "--tags",
                        "--end-of-options",
                        url,
                        "+refs/heads/*:refs/heads/*",
                    ],
                )?;
            }
            return Ok(());
        }
        if self.offline {
            return Err(Error::with_help(
                format!("--offline: no cached copy of '{}'", redact(url)),
                "run once without --offline to fill the cache",
            ));
        }
        if let Some(parent) = bare.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                Error::from(format!("Could not create '{}': {}", parent.display(), e))
            })?;
        }
        self.ui
            .status("Downloading", &format!("{} ({})", name, redact(url)));
        git(
            None,
            &[
                "clone",
                "--bare",
                "--quiet",
                "--end-of-options",
                url,
                &bare.to_string_lossy(),
            ],
        )?;
        Ok(())
    }

    fn resolve(&self, bare: &Path, name: &str, dep: &GitDependency) -> Result<String, Error> {
        let spec = match (&dep.rev, &dep.branch) {
            (Some(rev), _) => rev.clone(),
            (None, Some(branch)) => format!("refs/heads/{}", branch),
            (None, None) => "HEAD".to_string(),
        };
        let object = format!("{}^{{commit}}", spec);
        if let Ok(sha) = rev_parse(bare, &object) {
            return Ok(sha);
        }
        if let Some(rev) = dep.rev.as_deref().filter(|rev| is_full_sha(rev)) {
            if !self.offline {
                git(
                    Some(bare),
                    &["fetch", "--quiet", "--end-of-options", &dep.git, rev],
                )?;
                if let Ok(sha) = rev_parse(bare, &object) {
                    return Ok(sha);
                }
            }
        }
        Err(Error::from(format!(
            "Could not resolve '{}' in '{}' for dependency '{}'.",
            spec,
            redact(&dep.git),
            name
        )))
    }
}

fn rev_parse(bare: &Path, object: &str) -> Result<String, Error> {
    git(
        Some(bare),
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            object,
        ],
    )
}

fn is_full_sha(rev: &str) -> bool {
    (rev.len() == 40 || rev.len() == 64) && rev.chars().all(|c| c.is_ascii_hexdigit())
}

fn reject_option(name: &str, what: &str, value: &str) -> Result<(), Error> {
    if value.starts_with('-') {
        return Err(Error::from(format!(
            "Dependency '{}' has a {} starting with '-'.",
            name, what
        )));
    }
    Ok(())
}

pub fn redact(value: &str) -> String {
    match value.find("://") {
        Some(i) => {
            let start = i + 3;
            format!("{}{}", &value[..start], strip_userinfo(&value[start..]))
        }
        None => match value.find('@') {
            Some(at) if !value[..at].contains('/') && value[at..].contains(':') => {
                value[at + 1..].to_string()
            }
            _ => value.to_string(),
        },
    }
}

fn redact_text(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("://") {
        let start = i + 3;
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let end = tail
            .find(|c: char| c.is_whitespace() || c == '\'' || c == '"')
            .unwrap_or(tail.len());
        out.push_str(strip_userinfo(&tail[..end]));
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

fn strip_userinfo(rest: &str) -> &str {
    let authority_end = rest.find('/').unwrap_or(rest.len());
    match rest[..authority_end].rfind('@') {
        Some(at) => &rest[at + 1..],
        None => rest,
    }
}

fn materialize(bare: &Path, dir: &Path, sha: &str) -> Result<(), Error> {
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| Error::from(format!("Could not create '{}': {}", parent.display(), e)))?;
    }
    let result = git(
        None,
        &[
            "clone",
            "--quiet",
            "--no-checkout",
            "--local",
            &bare.to_string_lossy(),
            &dir.to_string_lossy(),
        ],
    )
    .and_then(|_| git(Some(dir), &["checkout", "--quiet", "--detach", sha]));
    result.map(|_| ()).inspect_err(|_| {
        let _ = fs::remove_dir_all(dir);
    })
}

fn git(dir: Option<&Path>, args: &[&str]) -> Result<String, Error> {
    let mut cmd = Command::new("git");
    if let Some(dir) = dir {
        cmd.arg("-C").arg(dir);
    }
    let output = cmd
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "git not found on PATH".to_string()
            } else {
                format!("Could not run git: {}", e)
            }
        })?;
    if !output.status.success() {
        let command: Vec<String> = args.iter().map(|arg| redact(arg)).collect();
        return Err(Error::from(format!(
            "git {} failed: {}",
            command.join(" "),
            redact_text(String::from_utf8_lossy(&output.stderr).trim())
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn short_hash(url: &str) -> String {
    Sha256::digest(url.as_bytes())
        .iter()
        .take(8)
        .map(|byte| format!("{:02x}", byte))
        .collect()
}

fn safe(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{commit, git_out, init_repo, temp_dir};
    use std::io;

    fn dep(url: &str, rev: Option<&str>, branch: Option<&str>) -> GitDependency {
        GitDependency {
            git: url.to_string(),
            rev: rev.map(str::to_string),
            branch: branch.map(str::to_string),
        }
    }

    fn file_url(repo: &Path) -> String {
        format!("file://{}", repo.display())
    }

    #[test]
    fn resolves_tag_branch_and_head_to_commits() -> io::Result<()> {
        let base = temp_dir("git_resolve")?;
        let repo = init_repo(&base, "lib")?;
        let first = commit(&repo, &[("src/a.nk", "1")], "one")?;
        crate::testutil::git_run(&repo, &["tag", "v1"])?;
        let second = commit(&repo, &[("src/a.nk", "2")], "two")?;
        let url = file_url(&repo);
        let store = Store::new(base.join("home"), false);

        let by_tag = store.checkout("lib", &dep(&url, Some("v1"), None));
        let by_branch = store.checkout("lib", &dep(&url, None, Some("main")));
        let by_head = store.checkout("lib", &dep(&url, None, None));
        let shas = (
            by_tag.map(|c| c.sha),
            by_branch.map(|c| c.sha),
            by_head.map(|c| c.sha),
        );
        fs::remove_dir_all(&base)?;
        let (tag, branch, head) = shas;
        assert_eq!(tag.map_err(io::Error::other)?, first);
        assert_eq!(branch.map_err(io::Error::other)?, second);
        assert_eq!(head.map_err(io::Error::other)?, second);
        Ok(())
    }

    #[test]
    fn checkout_materializes_the_resolved_commit() -> io::Result<()> {
        let base = temp_dir("git_materialize")?;
        let repo = init_repo(&base, "lib")?;
        let sha = commit(&repo, &[("src/a.nk", "hello")], "one")?;
        let store = Store::new(base.join("home"), false);

        let checkout = store
            .checkout("lib", &dep(&file_url(&repo), Some(&sha), None))
            .map_err(io::Error::other)?;
        let text = fs::read_to_string(checkout.dir.join("src/a.nk"));
        fs::remove_dir_all(&base)?;
        assert_eq!(checkout.sha, sha);
        assert_eq!(text?, "hello");
        Ok(())
    }

    #[test]
    fn offline_uses_cache_and_does_not_fetch() -> io::Result<()> {
        let base = temp_dir("git_offline")?;
        let repo = init_repo(&base, "lib")?;
        let first = commit(&repo, &[("src/a.nk", "1")], "one")?;
        let url = file_url(&repo);
        let home = base.join("home");
        let online = Store::new(home.clone(), false);
        online
            .checkout("lib", &dep(&url, None, Some("main")))
            .map_err(io::Error::other)?;

        let second = commit(&repo, &[("src/a.nk", "2")], "two")?;
        let offline = Store::new(home.clone(), true);
        let cached = offline
            .checkout("lib", &dep(&url, None, Some("main")))
            .map_err(io::Error::other)?;
        let refreshed = Store::new(home, false)
            .checkout("lib", &dep(&url, None, Some("main")))
            .map_err(io::Error::other)?;
        fs::remove_dir_all(&base)?;
        assert_eq!(cached.sha, first);
        assert_eq!(refreshed.sha, second);
        Ok(())
    }

    #[test]
    fn offline_without_cache_is_an_error() -> io::Result<()> {
        let base = temp_dir("git_nocache")?;
        let repo = init_repo(&base, "lib")?;
        commit(&repo, &[("src/a.nk", "1")], "one")?;
        let store = Store::new(base.join("home"), true);

        let err = store
            .checkout("lib", &dep(&file_url(&repo), None, None))
            .unwrap_err();
        fs::remove_dir_all(&base)?;
        assert!(err.contains("--offline"), "{err}");
        Ok(())
    }

    #[test]
    fn reuses_existing_checkout() -> io::Result<()> {
        let base = temp_dir("git_reuse")?;
        let repo = init_repo(&base, "lib")?;
        let sha = commit(&repo, &[("src/a.nk", "1")], "one")?;
        let url = file_url(&repo);
        let store = Store::new(base.join("home"), false);

        let first = store
            .checkout("lib", &dep(&url, Some(&sha), None))
            .map_err(io::Error::other)?;
        let marker = first.dir.join("marker");
        fs::write(&marker, "kept")?;
        let second = store
            .checkout("lib", &dep(&url, Some(&sha), None))
            .map_err(io::Error::other)?;
        let kept = marker.exists();
        fs::remove_dir_all(&base)?;
        assert_eq!(first.dir, second.dir);
        assert!(kept);
        Ok(())
    }

    #[test]
    fn rev_and_branch_together_is_an_error() -> io::Result<()> {
        let base = temp_dir("git_both")?;
        let store = Store::new(base.join("home"), false);

        let err = store
            .checkout("lib", &dep("file:///nowhere", Some("v1"), Some("main")))
            .unwrap_err();
        fs::remove_dir_all(&base)?;
        assert!(err.contains("both rev and branch"), "{err}");
        Ok(())
    }

    #[test]
    fn unknown_rev_is_an_error() -> io::Result<()> {
        let base = temp_dir("git_unknown")?;
        let repo = init_repo(&base, "lib")?;
        commit(&repo, &[("src/a.nk", "1")], "one")?;
        let store = Store::new(base.join("home"), false);

        let err = store
            .checkout("lib", &dep(&file_url(&repo), Some("no-such-tag"), None))
            .unwrap_err();
        fs::remove_dir_all(&base)?;
        assert!(err.contains("no-such-tag"), "{err}");
        Ok(())
    }

    #[test]
    fn head_of_repo_matches_rev_parse() -> io::Result<()> {
        let base = temp_dir("git_head")?;
        let repo = init_repo(&base, "lib")?;
        let sha = commit(&repo, &[("src/a.nk", "1")], "one")?;
        let store = Store::new(base.join("home"), false);

        let checkout = store
            .checkout("lib", &dep(&file_url(&repo), None, None))
            .map_err(io::Error::other)?;
        let expected = git_out(&repo, &["rev-parse", "HEAD"])?;
        fs::remove_dir_all(&base)?;
        assert_eq!(checkout.sha, sha);
        assert_eq!(checkout.sha, expected);
        Ok(())
    }

    #[test]
    fn option_like_values_are_rejected_before_git_runs() -> io::Result<()> {
        let base = temp_dir("git_option")?;
        let store = Store::new(base.join("home"), false);

        let rev = store.checkout("lib", &dep("file:///nowhere", Some("--output=x"), None));
        let branch = store.checkout("lib", &dep("file:///nowhere", None, Some("-x")));
        let url = store.checkout("lib", &dep("--upload-pack=touch x", None, None));
        fs::remove_dir_all(&base)?;
        assert!(rev.unwrap_err().contains("rev starting with '-'"));
        assert!(branch.unwrap_err().contains("branch starting with '-'"));
        assert!(url.unwrap_err().contains("url starting with '-'"));
        Ok(())
    }

    #[test]
    fn redact_removes_userinfo_from_urls() {
        assert_eq!(
            redact("https://user:pw@example.com/x.git"),
            "https://example.com/x.git"
        );
        assert_eq!(redact("ssh://git@host/x"), "ssh://host/x");
        assert_eq!(redact("file:///tmp/x"), "file:///tmp/x");
        assert_eq!(
            redact("git@github.com:org/repo.git"),
            "github.com:org/repo.git"
        );
        assert_eq!(redact("v1@x"), "v1@x");
        assert_eq!(redact("--output=x"), "--output=x");
    }

    #[test]
    fn redact_text_removes_userinfo_inside_messages() {
        assert_eq!(
            redact_text("fatal: 'https://u:p@h/x' failed"),
            "fatal: 'https://h/x' failed"
        );
    }

    #[test]
    fn offline_error_does_not_show_credentials() -> io::Result<()> {
        let base = temp_dir("git_redact_offline")?;
        let store = Store::new(base.join("home"), true);

        let err = store
            .checkout(
                "lib",
                &dep("https://user:secret@example.com/x.git", None, None),
            )
            .unwrap_err();
        fs::remove_dir_all(&base)?;
        assert!(err.contains("https://example.com/x.git"), "{err}");
        assert!(!err.contains("secret"), "{err}");
        Ok(())
    }

    #[test]
    fn git_failure_does_not_show_credentials() -> io::Result<()> {
        let base = temp_dir("git_redact_online")?;
        let store = Store::new(base.join("home"), false);

        let err = store
            .checkout(
                "lib",
                &dep("https://user:secret@127.0.0.1:9/x.git", None, None),
            )
            .unwrap_err();
        fs::remove_dir_all(&base)?;
        assert!(!err.contains("secret"), "{err}");
        Ok(())
    }

    #[test]
    fn unreachable_full_sha_is_fetched_once() -> io::Result<()> {
        let base = temp_dir("git_sha_fetch")?;
        let repo = init_repo(&base, "lib")?;
        let first = commit(&repo, &[("src/a.nk", "1")], "one")?;
        let second = commit(&repo, &[("src/a.nk", "2")], "two")?;
        crate::testutil::git_run(&repo, &["reset", "--quiet", "--hard", &first])?;
        let store = Store::new(base.join("home"), false);

        let checkout = store
            .checkout("lib", &dep(&file_url(&repo), Some(&second), None))
            .map_err(io::Error::other);
        fs::remove_dir_all(&base)?;
        assert_eq!(checkout?.sha, second);
        Ok(())
    }
}
