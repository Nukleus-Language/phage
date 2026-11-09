use crate::manifest;
use std::{
    fs,
    io::{self, ErrorKind},
    path::Path,
};

pub fn init(path: &Path) -> io::Result<()> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(
                ErrorKind::InvalidInput,
                "project path has no UTF-8 file name",
            )
        })?;
    fs::create_dir(path.join("src"))?;
    fs::write(path.join("src/main.nk"), "")?;
    fs::write(path.join("Phage.toml"), manifest::template(name).as_bytes())?;

    Ok(())
}

pub fn new(dir: &str) -> Result<(), String> {
    let path = Path::new(dir);
    fs::create_dir(path).map_err(|e| {
        if e.kind() == ErrorKind::AlreadyExists {
            format!("Directory '{}' already exists.", dir)
        } else {
            format!("Could not create directory '{}': {}", dir, e)
        }
    })?;
    init(path).map_err(|e| format!("Could not create project: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phage_toml_is_valid_toml() -> io::Result<()> {
        let base = std::env::temp_dir().join(format!(
            "phage-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&base)?;
        let project = base.join("demo");
        fs::create_dir(&project)?;

        init(&project)?;
        let text = fs::read_to_string(project.join("Phage.toml"))?;
        fs::remove_dir_all(&base)?;

        assert_eq!(
            text,
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\n"
        );
        Ok(())
    }
}
