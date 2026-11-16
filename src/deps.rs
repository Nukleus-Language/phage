use crate::manifest::{self, FILE_NAME, LintLevel, Manifest};
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
    pub root: PathBuf,
    pub deps: Vec<(String, PathBuf)>,
    pub lints: BTreeMap<String, LintLevel>,
    pub(crate) dir: PathBuf,
}

pub fn collect(root: &Path, manifest: &Manifest) -> Result<Graph, String> {
    let root = fs::canonicalize(root)
        .map_err(|e| format!("Could not resolve '{}': {}", root.display(), e))?;
    let mut walk = Walk::default();
    walk.stack
        .push((manifest.project.name.clone(), root.clone()));
    let deps = walk.visit(manifest, &root)?;
    Ok(Graph {
        deps,
        genes: walk.genes.into_values().collect(),
    })
}

#[derive(Default)]
struct Walk {
    genes: BTreeMap<String, Gene>,
    packages: BTreeMap<PathBuf, String>,
    stack: Vec<(String, PathBuf)>,
}

impl Walk {
    fn visit(&mut self, manifest: &Manifest, dir: &Path) -> Result<Vec<(String, PathBuf)>, String> {
        let mut declared = Vec::new();
        for (name, dep) in &manifest.dependencies {
            let candidate = dir.join(&dep.path);
            let canon = fs::canonicalize(&candidate).map_err(|e| {
                format!(
                    "Could not resolve dependency '{}' at '{}': {}",
                    name,
                    candidate.display(),
                    e
                )
            })?;

            if let Some(start) = self.stack.iter().position(|(_, seen)| *seen == canon) {
                let mut chain: Vec<&str> = self.stack[start..]
                    .iter()
                    .map(|(n, _)| n.as_str())
                    .collect();
                chain.push(name);
                return Err(format!("Dependency cycle: {}", chain.join(" -> ")));
            }

            if let Some(seen) = self.packages.get(&canon) {
                if seen != name {
                    return Err(format!(
                        "Dependency at '{}' is reached as both '{}' and '{}'",
                        canon.display(),
                        seen,
                        name
                    ));
                }
                declared.push((name.clone(), self.genes[name].root.clone()));
                continue;
            }

            if let Some(existing) = self.genes.get(name) {
                return Err(format!(
                    "Dependency '{}' resolves to both '{}' and '{}'",
                    name,
                    existing.dir.display(),
                    canon.display()
                ));
            }

            let manifest_path = canon.join(FILE_NAME);
            if !manifest_path.is_file() {
                return Err(format!(
                    "Dependency '{}' at '{}' has no {}",
                    name,
                    canon.display(),
                    FILE_NAME
                ));
            }
            let dep_manifest = manifest::load(&manifest_path)?;
            let root = canon.join(dep_manifest.lib_path());

            self.packages.insert(canon.clone(), name.clone());
            self.genes.insert(
                name.clone(),
                Gene {
                    name: name.clone(),
                    root: root.clone(),
                    deps: Vec::new(),
                    lints: dep_manifest.lints.clone(),
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

        let graph = collect(&root, &load_root(&root)?).map_err(io::Error::other)?;
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

        let graph = collect(&root, &load_root(&root)?).map_err(io::Error::other)?;
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

        let graph = collect(&root, &load_root(&root)?).map_err(io::Error::other)?;
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

        let graph = collect(&root, &load_root(&root)?).map_err(io::Error::other)?;
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

        let err = collect(&root, &load_root(&root)?).unwrap_err();
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

        let err = collect(&root, &load_root(&root)?).unwrap_err();
        fs::remove_dir_all(&base)?;
        assert_eq!(err, "Dependency cycle: a -> b -> a");
        Ok(())
    }

    #[test]
    fn dependency_without_manifest_is_an_error() -> io::Result<()> {
        let base = temp_dir("missing")?;
        let root = package(&base, "root", &[("a", "../a")], None)?;
        fs::create_dir_all(base.join("a"))?;

        let err = collect(&root, &load_root(&root)?).unwrap_err();
        fs::remove_dir_all(&base)?;
        assert!(err.contains("has no Phage.toml"), "{err}");
        assert!(err.contains("'a'"), "{err}");
        Ok(())
    }
}
