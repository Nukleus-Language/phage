use std::{
    env, fs,
    io::Result,
    path::PathBuf,
    process,
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
