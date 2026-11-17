use crate::error::Error;
use crate::{
    git::Store,
    manifest::{self, Dependency, FILE_NAME, GitDependency, LintLevel, Manifest},
};
use semver::{Version, VersionReq};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub struct Graph {
    pub deps: Vec<(String, PathBuf)>,
    pub genes: Vec<Gene>,
}

#[derive(Debug)]
pub struct Gene {
    pub name: String,
    pub version: String,
    pub root: PathBuf,
    pub deps: Vec<(String, PathBuf)>,
    pub lints: BTreeMap<String, LintLevel>,
    pub source: Source,
    pub(crate) dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Path,
    Git {
        url: String,
        sha: String,
    },
    Registry {
        index: String,
        version: String,
        checksum: String,
    },
}

pub fn collect(root: &Path, manifest: &Manifest, store: &Store) -> Result<Graph, Error> {
    let root = fs::canonicalize(root)
        .map_err(|e| Error::from(format!("Could not resolve '{}': {}", root.display(), e)))?;
    let mut walk = Walk {
        store,
        genes: BTreeMap::new(),
        packages: BTreeMap::new(),
        stack: Vec::new(),
        chosen: BTreeMap::new(),
    };
    walk.stack.push((manifest.name().to_string(), root.clone()));
    let deps = walk.visit(manifest, &root)?;
    Ok(Graph {
        deps,
        genes: walk.genes.into_values().collect(),
    })
}

struct Walk<'a> {
    store: &'a Store,
    genes: BTreeMap<String, Gene>,
    packages: BTreeMap<PathBuf, String>,
    stack: Vec<(String, PathBuf)>,
    chosen: BTreeMap<String, (Version, PathBuf, Source)>,
}

impl Walk<'_> {
    fn visit(&mut self, manifest: &Manifest, dir: &Path) -> Result<Vec<(String, PathBuf)>, Error> {
        let mut declared = Vec::new();
        for (name, dep) in &manifest.dependencies {
            let (canon, source) = self.locate(name, dep, dir)?;

            if let Some(start) = self.stack.iter().position(|(_, seen)| *seen == canon) {
                let mut chain: Vec<&str> = self.stack[start..]
                    .iter()
                    .map(|(n, _)| n.as_str())
                    .collect();
                chain.push(name);
                return Err(Error::from(format!(
                    "Dependency cycle: {}",
                    chain.join(" -> ")
                )));
            }

            if let Some(seen) = self.packages.get(&canon) {
                if seen != name {
                    return Err(Error::from(format!(
                        "Dependency at '{}' is reached as both '{}' and '{}'",
                        canon.display(),
                        seen,
                        name
                    )));
                }
                declared.push((name.clone(), self.genes[name].root.clone()));
                continue;
            }

            if let Some(existing) = self.genes.get(name) {
                return Err(Error::from(format!(
                    "Dependency '{}' resolves to both '{}' and '{}'",
                    name,
                    existing.dir.display(),
                    canon.display()
                )));
            }

            let manifest_path = canon.join(FILE_NAME);
            if !manifest_path.is_file() {
                return Err(Error::from(format!(
                    "Dependency '{}' at '{}' has no {}",
                    name,
                    canon.display(),
                    FILE_NAME
                )));
            }
            let dep_manifest = manifest::load(&manifest_path)?;
            let root = canon.join(dep_manifest.lib_path());

            self.packages.insert(canon.clone(), name.clone());
            self.genes.insert(
                name.clone(),
                Gene {
                    name: name.clone(),
                    version: dep_manifest.version().to_string(),
                    root: root.clone(),
                    deps: Vec::new(),
                    lints: dep_manifest.lints.clone(),
                    source,
                    dir: canon.clone(),
                },
            );
            self.stack.push((name.clone(), canon.clone()));
            let deps = self.visit(&dep_manifest, &canon)?;
            self.stack.pop();
            if let Some(gene) = self.genes.get_mut(name) {
                gene.deps = deps;
            }
            declared.push((name.clone(), root));
        }
        Ok(declared)
    }

    fn locate(
        &mut self,
        name: &str,
        dep: &Dependency,
        dir: &Path,
    ) -> Result<(PathBuf, Source), Error> {
        match dep {
            Dependency::Path(path) => {
                let candidate = dir.join(&path.path);
                let canon = fs::canonicalize(&candidate).map_err(|e| {
                    format!(
                        "Could not resolve dependency '{}' at '{}': {}",
                        name,
                        candidate.display(),
                        e
                    )
                })?;
                Ok((canon, Source::Path))
            }
            Dependency::Git(git) => self.locate_git(name, git),
            Dependency::Gene(gene) => {
                let (owner, repo) = gene.gene.split_once('/').ok_or_else(|| {
                    Error::with_help(
                        format!(
                            "'{}' has gene = \"{}\", which is not owner/name",
                            name, gene.gene
                        ),
                        "write gene = \"owner/name\"",
                    )
                })?;
                if owner.is_empty()
                    || repo.is_empty()
                    || repo.contains('/')
                    || !owner
                        .chars()
                        .chain(repo.chars())
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
                {
                    return Err(Error::from(format!(
                        "'{}' has gene = \"{}\", which is not owner/name",
                        name, gene.gene
                    )));
                }
                if gene.rev.is_some() && gene.tag.is_some() {
                    return Err(Error::from(format!(
                        "Dependency '{}' sets both rev and tag.",
                        name
                    )));
                }
                let git = GitDependency {
                    git: format!("https://github.com/{}/{}", owner, repo),
                    rev: gene.rev.clone().or_else(|| gene.tag.clone()),
                    branch: gene.branch.clone(),
                };
                self.locate_git(name, &git)
            }
            Dependency::Req(text) => self.locate_registry(name, text),
        }
    }

    fn locate_git(&self, name: &str, git: &GitDependency) -> Result<(PathBuf, Source), Error> {
        let checkout = self.store.checkout(name, git)?;
        let canon = fs::canonicalize(&checkout.dir)
            .map_err(|e| format!("Could not resolve '{}': {}", checkout.dir.display(), e))?;
        Ok((
            canon,
            Source::Git {
                url: git.git.clone(),
                sha: checkout.sha,
            },
        ))
    }

    /// One version per name: the first requirement picks the highest match; later ones must accept it.
    fn locate_registry(&mut self, name: &str, text: &str) -> Result<(PathBuf, Source), Error> {
        let req = VersionReq::parse(text).map_err(|e| {
            Error::with_help(
                format!("bad version requirement '{}' for '{}'", text, name),
                e.to_string(),
            )
        })?;
        if let Some((version, dir, source)) = self.chosen.get(name) {
            if !req.matches(version) {
                return Err(Error::with_help(
                    format!(
                        "'{}' is required as {} but {} was already chosen",
                        name, req, version
                    ),
                    "use one requirement per gene, or requirements that overlap",
                ));
            }
            return Ok((dir.clone(), source.clone()));
        }
        let pick = self.store.registry.pick(name, &req)?;
        let dir = self.store.registry.install(&pick)?;
        let canon = fs::canonicalize(&dir)
            .map_err(|e| format!("Could not resolve '{}': {}", dir.display(), e))?;
        let version = Version::parse(&pick.version)
            .map_err(|e| format!("bad version '{}' for '{}': {}", pick.version, name, e))?;
        let source = Source::Registry {
            index: self.store.registry.label(),
            version: pick.version.clone(),
            checksum: pick.cksum.clone(),
        };
        self.chosen
            .insert(name.to_string(), (version, canon.clone(), source.clone()));
        Ok((canon, source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::temp_dir;
    use std::io;

    fn package(
        base: &Path,
        name: &str,
        deps: &[(&str, &str)],
        lib: Option<&str>,
    ) -> io::Result<PathBuf> {
        let dir = base.join(name);
        fs::create_dir_all(dir.join("src"))?;
        let mut text = format!("[project]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n");
        if let Some(path) = lib {
            text += &format!("[lib]\npath = \"{path}\"\n\n");
        }
        text += "[dependencies]\n";
        for (dep, path) in deps {
            text += &format!("{dep} = {{ path = \"{path}\" }}\n");
        }
        fs::write(dir.join(FILE_NAME), text)?;
        Ok(dir)
    }

    fn load_root(dir: &Path) -> io::Result<Manifest> {
        manifest::load(&dir.join(FILE_NAME)).map_err(io::Error::other)
    }

    fn names(genes: &[Gene]) -> Vec<&str> {
        genes.iter().map(|gene| gene.name.as_str()).collect()
    }

    #[test]
    fn dependency_lib_path_overrides_default_root() -> io::Result<()> {
        let base = temp_dir("lib")?;
        let root = package(&base, "root", &[("dep", "../dep")], None)?;
        let dep = package(&base, "dep", &[], Some("src/x.nk"))?;
        let expected = fs::canonicalize(&dep)?.join("src/x.nk");

        let graph = collect(
            &root,
            &load_root(&root)?,
            &Store::new(base.join("home"), false),
        )
        .map_err(io::Error::other)?;
        fs::remove_dir_all(&base)?;
        assert_eq!(graph.deps, vec![("dep".to_string(), expected)]);
        Ok(())
    }

    #[test]
    fn three_level_chain_passes_every_gene() -> io::Result<()> {
        let base = temp_dir("chain")?;
        let root = package(&base, "root", &[("a", "../a")], None)?;
        package(&base, "a", &[("b", "../b")], None)?;
        package(&base, "b", &[("c", "../c")], None)?;
        package(&base, "c", &[], None)?;

        let graph = collect(
            &root,
            &load_root(&root)?,
            &Store::new(base.join("home"), false),
        )
        .map_err(io::Error::other)?;
        fs::remove_dir_all(&base)?;
        assert_eq!(names(&graph.genes), ["a", "b", "c"]);
        assert!(
            graph
                .genes
                .iter()
                .all(|gene| gene.root.ends_with("src/lib.nk"))
        );
        Ok(())
    }

    #[test]
    fn diamond_passes_shared_dependency_once() -> io::Result<()> {
        let base = temp_dir("diamond")?;
        let root = package(&base, "root", &[("a", "../a")], None)?;
        package(&base, "a", &[("b", "../b"), ("c", "../c")], None)?;
        package(&base, "b", &[("d", "../d")], None)?;
        package(&base, "c", &[("d", "../d")], None)?;
        package(&base, "d", &[], None)?;

        let graph = collect(
            &root,
            &load_root(&root)?,
            &Store::new(base.join("home"), false),
        )
        .map_err(io::Error::other)?;
        fs::remove_dir_all(&base)?;
        assert_eq!(names(&graph.genes), ["a", "b", "c", "d"]);
        Ok(())
    }

    #[test]
    fn each_gene_records_only_its_own_deps_and_lints() -> io::Result<()> {
        let base = temp_dir("per_gene")?;
        let root = package(&base, "root", &[("a", "../a")], None)?;
        let a = package(&base, "a", &[("b", "../b"), ("c", "../c")], None)?;
        fs::write(
            a.join(FILE_NAME),
            "[project]\nname = \"a\"\nversion = \"0.1.0\"\n\n[lints]\nnever_mutated = \"deny\"\n\n[dependencies]\nb = { path = \"../b\" }\nc = { path = \"../c\" }\n",
        )?;
        package(&base, "b", &[], None)?;
        package(&base, "c", &[("b", "../b")], None)?;
        let a_root = fs::canonicalize(&a)?.join("src/lib.nk");
        let b_root = fs::canonicalize(base.join("b"))?.join("src/lib.nk");
        let c_root = fs::canonicalize(base.join("c"))?.join("src/lib.nk");

        let graph = collect(
            &root,
            &load_root(&root)?,
            &Store::new(base.join("home"), false),
        )
        .map_err(io::Error::other)?;
        fs::remove_dir_all(&base)?;
        assert_eq!(graph.deps, vec![("a".to_string(), a_root)]);
        let [gene_a, gene_b, gene_c] = graph.genes.as_slice() else {
            panic!("three genes");
        };
        assert_eq!(gene_a.name, "a");
        assert_eq!(
            gene_a.deps,
            vec![
                ("b".to_string(), b_root.clone()),
                ("c".to_string(), c_root.clone())
            ]
        );
        assert_eq!(
            gene_a.lints,
            BTreeMap::from([("never_mutated".to_string(), LintLevel::Deny)])
        );
        assert_eq!(gene_b.name, "b");
        assert!(gene_b.deps.is_empty() && gene_b.lints.is_empty());
        assert_eq!(gene_c.deps, vec![("b".to_string(), b_root)]);
        assert!(gene_c.lints.is_empty());
        Ok(())
    }

    #[test]
    fn same_name_from_two_paths_is_an_error() -> io::Result<()> {
        let base = temp_dir("clash")?;
        let root = package(&base, "root", &[("a", "../a"), ("b", "../b")], None)?;
        package(&base, "a", &[("x", "../x1")], None)?;
        package(&base, "b", &[("x", "../x2")], None)?;
        package(&base, "x1", &[], None)?;
        package(&base, "x2", &[], None)?;
        let x1 = fs::canonicalize(base.join("x1"))?;
        let x2 = fs::canonicalize(base.join("x2"))?;

        let err = collect(
            &root,
            &load_root(&root)?,
            &Store::new(base.join("home"), false),
        )
        .unwrap_err();
        fs::remove_dir_all(&base)?;
        assert!(err.contains(&x1.display().to_string()), "{err}");
        assert!(err.contains(&x2.display().to_string()), "{err}");
        Ok(())
    }

    #[test]
    fn dependency_cycle_is_an_error() -> io::Result<()> {
        let base = temp_dir("cycle")?;
        let root = package(&base, "root", &[("a", "../a")], None)?;
        package(&base, "a", &[("b", "../b")], None)?;
        package(&base, "b", &[("a", "../a")], None)?;

        let err = collect(
            &root,
            &load_root(&root)?,
            &Store::new(base.join("home"), false),
        )
        .unwrap_err();
        fs::remove_dir_all(&base)?;
        assert_eq!(err, "Dependency cycle: a -> b -> a");
        Ok(())
    }

    #[test]
    fn dependency_without_manifest_is_an_error() -> io::Result<()> {
        let base = temp_dir("missing")?;
        let root = package(&base, "root", &[("a", "../a")], None)?;
        fs::create_dir_all(base.join("a"))?;

        let err = collect(
            &root,
            &load_root(&root)?,
            &Store::new(base.join("home"), false),
        )
        .unwrap_err();
        fs::remove_dir_all(&base)?;
        assert!(err.contains("has no Phage.toml"), "{err}");
        assert!(err.contains("'a'"), "{err}");
        Ok(())
    }

    #[test]
    fn git_dependencies_of_git_dependencies_are_walked() -> io::Result<()> {
        use crate::testutil::{commit, git_out, init_repo};
        let base = temp_dir("git_chain")?;
        let repo_b = init_repo(&base, "b")?;
        let b_manifest = "[project]\nname = \"b\"\nversion = \"0.1.0\"\n";
        commit(
            &repo_b,
            &[("Phage.toml", b_manifest), ("src/lib.nk", "")],
            "b",
        )?;
        let sha_b = git_out(&repo_b, &["rev-parse", "HEAD"])?;
        let url_b = format!("file://{}", repo_b.display());

        let repo_a = init_repo(&base, "a")?;
        let a_manifest = format!(
            "[project]\nname = \"a\"\nversion = \"0.1.0\"\n\n[dependencies]\nb = {{ git = \"{url_b}\" }}\n"
        );
        let sha_a = commit(
            &repo_a,
            &[("Phage.toml", a_manifest.as_str()), ("src/lib.nk", "")],
            "a",
        )?;
        let url_a = format!("file://{}", repo_a.display());

        let root = package(&base, "root", &[], None)?;
        let root_manifest = format!(
            "[project]\nname = \"root\"\nversion = \"0.1.0\"\n\n[dependencies]\na = {{ git = \"{url_a}\" }}\n"
        );
        fs::write(root.join(FILE_NAME), root_manifest)?;
        let store = Store::new(base.join("home"), false);

        let graph = collect(&root, &load_root(&root)?, &store);
        let result = graph.map(|g| {
            (
                g.genes
                    .iter()
                    .map(|gene| gene.name.clone())
                    .collect::<Vec<_>>(),
                g.genes
                    .iter()
                    .map(|gene| gene.source.clone())
                    .collect::<Vec<_>>(),
            )
        });
        fs::remove_dir_all(&base)?;
        let (names, sources) = result.map_err(io::Error::other)?;
        assert_eq!(names, ["a", "b"]);
        assert_eq!(
            sources,
            vec![
                Source::Git {
                    url: url_a,
                    sha: sha_a
                },
                Source::Git {
                    url: url_b,
                    sha: sha_b
                },
            ]
        );
        Ok(())
    }
}
