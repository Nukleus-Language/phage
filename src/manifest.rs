use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub const FILE_NAME: &str = "Phage.toml";

#[derive(Debug, Deserialize, Serialize)]
pub struct Manifest {
    pub project: Project,
    #[serde(default)]
    pub dependencies: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Project {
    #[serde(default)]
    pub name: String,
    pub version: String,
}

pub fn template(name: &str) -> Result<String, toml::ser::Error> {
    toml::to_string(&Manifest {
        project: Project {
            name: name.to_string(),
            version: "0.1.0".to_string(),
        },
        dependencies: BTreeMap::new(),
    })
}

pub fn find_manifest(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .map(|dir| dir.join(FILE_NAME))
        .find(|candidate| candidate.is_file())
}

pub fn load(path: &Path) -> Result<Manifest, String> {
    let text = fs::read_to_string(path)
        .map_err(|e| format!("Could not read '{}': {}", path.display(), e))?;
    let manifest: Manifest = toml::from_str(&text)
        .map_err(|e| format!("Could not parse '{}': {}", path.display(), e))?;
    if manifest.project.name.is_empty() {
        return Err(format!("'{}' is missing project.name", path.display()));
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::temp_dir;

    #[test]
    fn finds_manifest_in_ancestor() -> std::io::Result<()> {
        let base = temp_dir("find")?;
        let nested = base.join("a/b");
        fs::create_dir_all(&nested)?;
        fs::write(base.join(FILE_NAME), "")?;

        assert_eq!(find_manifest(&nested), Some(base.join(FILE_NAME)));
        fs::remove_dir_all(&base)?;
        Ok(())
    }

    #[test]
    fn template_escapes_quotes_and_backslashes() -> Result<(), Box<dyn std::error::Error>> {
        let name = r#"we"ird\name"#;
        let text = template(name)?;
        let manifest: Manifest = toml::from_str(&text)?;
        assert_eq!(manifest.project.name, name);
        assert_eq!(manifest.project.version, "0.1.0");
        Ok(())
    }

    #[test]
    fn load_reports_parse_error() -> std::io::Result<()> {
        let base = temp_dir("parse")?;
        let path = base.join(FILE_NAME);
        fs::write(&path, "[project\nname = ")?;

        let err = load(&path).unwrap_err();
        assert!(err.starts_with("Could not parse"), "{err}");
        fs::remove_dir_all(&base)?;
        Ok(())
    }

    #[test]
    fn load_reports_missing_name() -> std::io::Result<()> {
        let base = temp_dir("noname")?;
        let path = base.join(FILE_NAME);
        fs::write(&path, "[project]\nversion = \"0.1.0\"\n")?;

        let err = load(&path).unwrap_err();
        assert!(err.contains("missing project.name"), "{err}");
        fs::remove_dir_all(&base)?;
        Ok(())
    }
}
