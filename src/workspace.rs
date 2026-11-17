use crate::{
    error::Error,
    manifest::{self, FILE_NAME, Manifest, find_manifest},
};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub struct Package {
    pub dir: PathBuf,
    pub manifest: Manifest,
}

pub struct Workspace {
    /// Holds the target directory and Phage.lock.
    pub root: PathBuf,
    /// The manifest that holds [profile.*]; for a single package, the package itself.
    pub manifest: Manifest,
    pub packages: Vec<Package>,
}

impl Workspace {
    pub fn discover(cwd: &Path) -> Result<Workspace, Error> {
        let nearest = find_manifest(cwd).ok_or_else(|| missing_manifest(cwd))?;
        let nearest_dir = fs::canonicalize(nearest.parent().unwrap_or(Path::new(".")))
            .map_err(|e| Error::from(format!("Could not resolve '{}': {}", cwd.display(), e)))?;

        for dir in nearest_dir.ancestors() {
            let candidate = dir.join(FILE_NAME);
            if !candidate.is_file() {
                continue;
            }
            let manifest = manifest::load(&candidate)?;
            let Some(workspace) = &manifest.workspace else {
                continue;
            };
            let members = expand_members(dir, &workspace.members)?;
            if dir == nearest_dir || members.contains(&nearest_dir) {
                return from_workspace(dir, manifest, members);
            }
        }

        let manifest = manifest::load(&nearest)?;
        if manifest.project.is_none() {
            return Err(Error::from(format!(
                "'{}' has no [project]",
                nearest.display()
            )));
        }
        let package = Package {
            dir: nearest_dir.clone(),
            manifest,
        };
        let manifest = manifest::load(&nearest)?;
        Ok(Workspace {
            root: nearest_dir,
            manifest,
            packages: vec![package],
        })
    }

    pub fn select(&self, names: &[String]) -> Result<Vec<&Package>, Error> {
        if names.is_empty() {
            return Ok(self.packages.iter().collect());
        }
        names
            .iter()
            .map(|name| {
                self.packages
                    .iter()
                    .find(|package| package.manifest.name() == name)
                    .ok_or_else(|| {
                        Error::with_help(
                            format!("no package named '{}' in the workspace", name),
                            format!("packages: {}", self.package_names().join(", ")),
                        )
                    })
            })
            .collect()
    }

    pub fn package_names(&self) -> Vec<&str> {
        self.packages
            .iter()
            .map(|package| package.manifest.name())
            .collect()
    }
}

pub fn missing_manifest(cwd: &Path) -> Error {
    Error::with_help(
        format!(
            "No {} found in '{}' or any parent directory.",
            FILE_NAME,
            cwd.display()
        ),
        "run 'phage init' or 'phage new <name>' to create a project",
    )
}

fn from_workspace(
    dir: &Path,
    manifest: Manifest,
    members: Vec<PathBuf>,
) -> Result<Workspace, Error> {
    let mut packages = Vec::new();
    if manifest.project.is_some() {
        packages.push(Package {
            dir: dir.to_path_buf(),
            manifest: manifest::load(&dir.join(FILE_NAME))?,
        });
    }
    for member in members {
        let loaded = manifest::load(&member.join(FILE_NAME))?;
        if loaded.project.is_none() {
            return Err(Error::from(format!(
                "workspace member '{}' has no [project]",
                member.display()
            )));
        }
        if loaded.workspace.is_some() {
            return Err(Error::from(format!(
                "workspace member '{}' cannot be a workspace itself",
                member.display()
            )));
        }
        packages.push(Package {
            dir: member,
            manifest: loaded,
        });
    }
    for (index, package) in packages.iter().enumerate() {
        if let Some(other) = packages[..index]
            .iter()
            .find(|seen| seen.manifest.name() == package.manifest.name())
        {
            return Err(Error::from(format!(
                "two workspace packages are named '{}': '{}' and '{}'",
                package.manifest.name(),
                other.dir.display(),
                package.dir.display()
            )));
        }
    }
    Ok(Workspace {
        root: dir.to_path_buf(),
        manifest,
        packages,
    })
}

fn expand_members(root: &Path, patterns: &[String]) -> Result<Vec<PathBuf>, Error> {
    let mut found = Vec::new();
    for pattern in patterns {
        let (parent, last) = match pattern.rsplit_once('/') {
            Some((parent, last)) => (parent, last),
            None => ("", pattern.as_str()),
        };
        if parent.contains('*') {
            return Err(unsupported(pattern));
        }
        if last == "*" {
            let dir = root.join(parent);
            let entries = fs::read_dir(&dir).map_err(|e| {
                Error::from(format!(
                    "Could not read workspace member directory '{}': {}",
                    dir.display(),
                    e
                ))
            })?;
            let mut matched: Vec<PathBuf> = entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.join(FILE_NAME).is_file())
                .collect();
            matched.sort();
            found.extend(matched);
        } else if last.contains('*') {
            return Err(unsupported(pattern));
        } else {
            let dir = root.join(pattern);
            if !dir.join(FILE_NAME).is_file() {
                return Err(Error::from(format!(
                    "workspace member '{}' has no {}",
                    pattern, FILE_NAME
                )));
            }
            found.push(dir);
        }
    }
    let mut resolved = Vec::new();
    for dir in found {
        let canon = fs::canonicalize(&dir)
            .map_err(|e| Error::from(format!("Could not resolve '{}': {}", dir.display(), e)))?;
        if !resolved.contains(&canon) {
            resolved.push(canon);
        }
    }
    Ok(resolved)
}

fn unsupported(pattern: &str) -> Error {
    Error::with_help(
        format!("unsupported workspace member pattern '{}'", pattern),
        "use a literal path, or a single '*' as the last component",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::temp_dir;
    use std::io;

    fn write_manifest(dir: &Path, text: &str) -> io::Result<()> {
        fs::create_dir_all(dir)?;
        fs::write(dir.join(FILE_NAME), text)
    }

    fn package_text(name: &str) -> String {
        format!("[project]\nname = \"{name}\"\nversion = \"0.1.0\"\n")
    }

    #[test]
    fn glob_member_expands_to_every_child_with_a_manifest() -> io::Result<()> {
        let base = temp_dir("ws_glob")?;
        write_manifest(&base, "[workspace]\nmembers = [\"b/*\"]\n")?;
        write_manifest(&base.join("b/one"), &package_text("one"))?;
        write_manifest(&base.join("b/two"), &package_text("two"))?;
        fs::create_dir_all(base.join("b/not-a-package"))?;

        let ws = Workspace::discover(&base).map_err(io::Error::other);
        fs::remove_dir_all(&base)?;
        let ws = ws?;
        assert_eq!(ws.package_names(), ["one", "two"]);
        Ok(())
    }

    #[test]
    fn workspace_only_root_builds_members_and_no_root_package() -> io::Result<()> {
        let base = temp_dir("ws_only")?;
        write_manifest(&base, "[workspace]\nmembers = [\"a\"]\n")?;
        write_manifest(&base.join("a"), &package_text("a"))?;

        let ws = Workspace::discover(&base.join("a")).map_err(io::Error::other);
        fs::remove_dir_all(&base)?;
        let ws = ws?;
        assert_eq!(ws.package_names(), ["a"]);
        assert!(ws.root.ends_with(base.file_name().unwrap_or_default()));
        Ok(())
    }

    #[test]
    fn root_that_is_also_a_package_comes_first() -> io::Result<()> {
        let base = temp_dir("ws_both")?;
        write_manifest(
            &base,
            &format!("{}\n[workspace]\nmembers = [\"a\"]\n", package_text("root")),
        )?;
        write_manifest(&base.join("a"), &package_text("a"))?;

        let ws = Workspace::discover(&base).map_err(io::Error::other);
        fs::remove_dir_all(&base)?;
        assert_eq!(ws?.package_names(), ["root", "a"]);
        Ok(())
    }

    #[test]
    fn select_by_name_and_reject_unknown() -> io::Result<()> {
        let base = temp_dir("ws_select")?;
        write_manifest(&base, "[workspace]\nmembers = [\"a\", \"b\"]\n")?;
        write_manifest(&base.join("a"), &package_text("a"))?;
        write_manifest(&base.join("b"), &package_text("b"))?;

        let ws = Workspace::discover(&base).map_err(io::Error::other)?;
        let picked = ws.select(&["b".to_string()]).map_err(io::Error::other);
        let missing = ws.select(&["c".to_string()]).unwrap_err();
        fs::remove_dir_all(&base)?;
        assert_eq!(picked?.len(), 1);
        assert!(missing.contains("no package named 'c'"), "{missing}");
        assert_eq!(missing.help.as_deref(), Some("packages: a, b"));
        Ok(())
    }

    #[test]
    fn member_inside_workspace_finds_the_workspace_root() -> io::Result<()> {
        let base = temp_dir("ws_nested_cwd")?;
        write_manifest(&base, "[workspace]\nmembers = [\"a\"]\n")?;
        write_manifest(&base.join("a"), &package_text("a"))?;
        fs::create_dir_all(base.join("a/src"))?;

        let ws = Workspace::discover(&base.join("a/src")).map_err(io::Error::other);
        fs::remove_dir_all(&base)?;
        assert_eq!(ws?.packages.len(), 1);
        Ok(())
    }

    #[test]
    fn duplicate_member_names_are_an_error() -> io::Result<()> {
        let base = temp_dir("ws_dup")?;
        write_manifest(&base, "[workspace]\nmembers = [\"a\", \"b\"]\n")?;
        write_manifest(&base.join("a"), &package_text("same"))?;
        write_manifest(&base.join("b"), &package_text("same"))?;

        let err = Workspace::discover(&base).err();
        fs::remove_dir_all(&base)?;
        assert!(err.is_some_and(|e| e.contains("two workspace packages are named 'same'")));
        Ok(())
    }

    #[test]
    fn member_without_manifest_is_an_error() -> io::Result<()> {
        let base = temp_dir("ws_missing")?;
        write_manifest(&base, "[workspace]\nmembers = [\"gone\"]\n")?;

        let err = Workspace::discover(&base).err();
        fs::remove_dir_all(&base)?;
        assert!(err.is_some_and(|e| e.contains("has no Phage.toml")));
        Ok(())
    }

    #[test]
    fn nested_glob_is_rejected() -> io::Result<()> {
        let base = temp_dir("ws_nested_glob")?;
        write_manifest(&base, "[workspace]\nmembers = [\"x/*/y\"]\n")?;

        let err = Workspace::discover(&base).err();
        fs::remove_dir_all(&base)?;
        assert!(err.is_some_and(|e| e.contains("unsupported workspace member pattern")));
        Ok(())
    }
}
