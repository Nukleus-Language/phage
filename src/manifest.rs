use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub const FILE_NAME: &str = "Phage.toml";
const DEFAULT_LIB: &str = "src/lib.nk";

#[derive(Debug, Deserialize, Serialize)]
pub struct Manifest {
    pub project: Project,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lib: Option<Lib>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, Dependency>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lints: BTreeMap<String, LintLevel>,
}

impl Manifest {
    pub fn lib_path(&self) -> &Path {
        self.lib
            .as_ref()
            .map_or(Path::new(DEFAULT_LIB), |lib| lib.path.as_path())
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Lib {
    #[serde(default = "default_lib_path")]
    pub path: PathBuf,
}

fn default_lib_path() -> PathBuf {
    PathBuf::from(DEFAULT_LIB)
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Project {
    #[serde(default)]
    pub name: String,
    pub version: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LintLevel {
    Allow,
    Warn,
    Deny,
}

impl LintLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            LintLevel::Allow => "allow",
            LintLevel::Warn => "warn",
            LintLevel::Deny => "deny",
        }
    }
}

pub fn template(name: &str) -> Result<String, toml::ser::Error> {
    toml::to_string(&Manifest {
        project: Project {
            name: name.to_string(),
            version: "0.1.0".to_string(),
        },
        lib: None,
        dependencies: BTreeMap::new(),
        lints: BTreeMap::new(),
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

    #[test]
    fn parses_lints_table() -> Result<(), toml::de::Error> {
        let manifest: Manifest = toml::from_str(
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[lints]\nnever_mutated = \"warn\"\nfuture_lint = \"deny\"\nold = \"allow\"\n",
        )?;
        assert_eq!(
            manifest.lints,
            BTreeMap::from([
                ("future_lint".to_string(), LintLevel::Deny),
                ("never_mutated".to_string(), LintLevel::Warn),
                ("old".to_string(), LintLevel::Allow),
            ])
        );
        Ok(())
    }

    #[test]
    fn rejects_unknown_lint_level() {
        let err = toml::from_str::<Manifest>(
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[lints]\nnever_mutated = \"forbid\"\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("forbid"), "{err}");
    }

    #[test]
    fn rejects_non_string_lint_level() {
        let result = toml::from_str::<Manifest>(
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[lints]\nnever_mutated = true\n",
        );
        assert!(result.is_err());
    }

    #[test]
    fn parses_path_dependency() -> Result<(), toml::de::Error> {
        let manifest: Manifest = toml::from_str(
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\njson = { path = \"../json\" }\n",
        )?;
        assert_eq!(manifest.dependencies["json"].path, PathBuf::from("../json"));
        Ok(())
    }

    #[test]
    fn rejects_dependency_without_path_source() {
        let result = toml::from_str::<Manifest>(
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\njson = { git = \"https://example.com/json\" }\n",
        );
        assert!(result.is_err());
    }

    #[test]
    fn lib_path_defaults_to_src_lib() -> Result<(), toml::de::Error> {
        let manifest: Manifest =
            toml::from_str("[project]\nname = \"demo\"\nversion = \"0.1.0\"\n")?;
        assert_eq!(manifest.lib_path(), Path::new("src/lib.nk"));
        Ok(())
    }

    #[test]
    fn lib_path_reads_override() -> Result<(), toml::de::Error> {
        let manifest: Manifest = toml::from_str(
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[lib]\npath = \"src/x.nk\"\n",
        )?;
        assert_eq!(manifest.lib_path(), Path::new("src/x.nk"));
        Ok(())
    }

    #[test]
    fn template_omits_empty_lints_table() -> Result<(), toml::ser::Error> {
        let text = template("demo")?;
        assert!(!text.contains("[lints]"), "{text}");
        Ok(())
    }
}
