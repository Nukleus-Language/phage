use std::{
    env, fs,
    io::{self, Result},
    path::{Path, PathBuf},
    process::{self, Command},
    time::{SystemTime, UNIX_EPOCH},
};

pub fn temp_dir(tag: &str) -> Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = env::temp_dir().join(format!("phage-{}-{}-{}", tag, process::id(), nanos));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn git_out(dir: &Path, args: &[&str]) -> io::Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn git_run(dir: &Path, args: &[&str]) -> io::Result<()> {
    git_out(dir, args).map(|_| ())
}

pub fn init_repo(base: &Path, name: &str) -> io::Result<PathBuf> {
    let repo = base.join(name);
    fs::create_dir_all(&repo)?;
    git_run(&repo, &["init", "--quiet"])?;
    git_run(&repo, &["symbolic-ref", "HEAD", "refs/heads/main"])?;
    Ok(repo)
}

pub fn commit(repo: &Path, files: &[(&str, &str)], message: &str) -> io::Result<String> {
    for (path, text) in files {
        let full = repo.join(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(full, text)?;
    }
    git_run(repo, &["add", "-A"])?;
    git_run(repo, &["commit", "--quiet", "-m", message])?;
    git_out(repo, &["rev-parse", "HEAD"])
}
