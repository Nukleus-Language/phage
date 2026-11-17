use crate::error::Error;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<Project>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<Workspace>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lib: Option<Lib>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, Dependency>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lints: BTreeMap<String, LintLevel>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profile: BTreeMap<String, ProfileDef>,
}

impl Manifest {
    pub fn lib_path(&self) -> &Path {
        self.lib
            .as_ref()
            .map_or(Path::new(DEFAULT_LIB), |lib| lib.path.as_path())
    }

    pub fn name(&self) -> &str {
        self.project.as_ref().map_or("", |project| &project.name)
    }

    pub fn version(&self) -> &str {
        self.project.as_ref().map_or("", |project| &project.version)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    #[serde(default)]
    pub members: Vec<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileDef {
    #[serde(default, rename = "opt-level", skip_serializing_if = "Option::is_none")]
    pub opt_level: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherits: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lints: BTreeMap<String, LintLevel>,
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
#[serde(untagged)]
pub enum Dependency {
    Git(GitDependency),
    Gene(GeneDependency),
    Path(PathDependency),
    Req(String),
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GeneDependency {
    pub gene: String,
    #[serde(default)]
    pub rev: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PathDependency {
    pub path: PathBuf,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GitDependency {
    pub git: String,
    #[serde(default)]
    pub rev: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
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
        project: Some(Project {
            name: name.to_string(),
            version: "0.1.0".to_string(),
        }),
        workspace: None,
        lib: None,
        dependencies: BTreeMap::new(),
        lints: BTreeMap::new(),
        profile: BTreeMap::new(),
    })
}

pub fn find_manifest(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .map(|dir| dir.join(FILE_NAME))
        .find(|candidate| candidate.is_file())
}

pub fn load(path: &Path) -> Result<Manifest, Error> {
    let text = fs::read_to_string(path)
        .map_err(|e| Error::from(format!("Could not read '{}': {}", path.display(), e)))?;
    let manifest: Manifest = toml::from_str(&text)
        .map_err(|e| Error::from(format!("Could not parse '{}': {}", path.display(), e)))?;
    match &manifest.project {
        Some(project) if project.name.is_empty() => {
            return Err(Error::from(format!(
                "'{}' is missing project.name",
                path.display()
            )));
        }
        None if manifest.workspace.is_none() => {
            return Err(Error::from(format!(
                "'{}' has neither [project] nor [workspace]",
                path.display()
            )));
        }
        _ => {}
    }
    for (name, def) in &manifest.profile {
        if def.opt_level.is_some_and(|level| level > 3) {
            return Err(Error::from(format!(
                "'{}': profile '{}' opt-level must be 0 to 3",
                path.display(),
                name
            )));
        }
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
        assert_eq!(manifest.name(), name);
        assert_eq!(manifest.version(), "0.1.0");
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
        let Dependency::Path(json) = &manifest.dependencies["json"] else {
            panic!("path dependency");
        };
        assert_eq!(json.path, PathBuf::from("../json"));
        Ok(())
    }

    #[test]
    fn parses_git_dependency_with_rev() -> Result<(), toml::de::Error> {
        let manifest: Manifest = toml::from_str(
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\njson = { git = \"https://example.com/json\", rev = \"v1\" }\n",
        )?;
        let Dependency::Git(json) = &manifest.dependencies["json"] else {
            panic!("git dependency");
        };
        assert_eq!(json.git, "https://example.com/json");
        assert_eq!(json.rev.as_deref(), Some("v1"));
        assert_eq!(json.branch, None);
        Ok(())
    }

    #[test]
    fn rejects_dependency_with_path_and_git() {
        let result = toml::from_str::<Manifest>(
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\njson = { path = \"../json\", git = \"https://example.com/json\" }\n",
        );
        assert!(result.is_err());
    }

    #[test]
    fn rejects_dependency_without_source() {
        let result = toml::from_str::<Manifest>(
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\njson = { rev = \"v1\" }\n",
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
    fn workspace_only_manifest_loads_without_project() -> std::io::Result<()> {
        let base = temp_dir("ws_only_manifest")?;
        let path = base.join(FILE_NAME);
        fs::write(&path, "[workspace]\nmembers = [\"a\"]\n")?;
        let loaded = load(&path).map_err(std::io::Error::other);
        fs::remove_dir_all(&base)?;
        let manifest = loaded?;
        assert!(manifest.project.is_none());
        assert_eq!(
            manifest.workspace.map(|w| w.members),
            Some(vec!["a".to_string()])
        );
        Ok(())
    }

    #[test]
    fn load_rejects_opt_level_above_three() -> std::io::Result<()> {
        let base = temp_dir("opt_level")?;
        let path = base.join(FILE_NAME);
        fs::write(
            &path,
            "[project]\nname = \"x\"\nversion = \"0.1.0\"\n\n[profile.dev]\nopt-level = 4\n",
        )?;
        let err = load(&path).unwrap_err();
        fs::remove_dir_all(&base)?;
        assert!(err.contains("opt-level must be 0 to 3"), "{err}");
        Ok(())
    }

    #[test]
    fn template_omits_empty_lints_table() -> Result<(), toml::ser::Error> {
        let text = template("demo")?;
        assert!(!text.contains("[lints]"), "{text}");
        Ok(())
    }
}
