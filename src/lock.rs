use crate::error::Error;
use crate::{
    deps::{Gene, Graph, Source},
    git::redact,
    ui::Ui,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Component, Path, PathBuf},
};

pub const FILE_NAME: &str = "Phage.lock";

#[derive(Serialize)]
struct Lock {
    gene: Vec<Entry>,
}

#[derive(Serialize)]
struct Entry {
    name: String,
    source: String,
    version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    checksum: Option<String>,
    sha256: String,
}

#[derive(Deserialize)]
struct LockFile {
    #[serde(default)]
    gene: Vec<Record>,
}

#[derive(Deserialize)]
struct Record {
    name: String,
    source: String,
    #[serde(default)]
    version: String,
}

/// A gene's recorded source and version, used to keep locked versions on the next resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub source: String,
    pub version: String,
}

/// Reads Phage.lock under `root`; a missing file pins nothing.
pub fn pins(root: &Path) -> Result<BTreeMap<String, Pin>, Error> {
    let path = root.join(FILE_NAME);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => {
            return Err(Error::from(format!(
                "Could not read '{}': {}",
                path.display(),
                e
            )));
        }
    };
    let file: LockFile = toml::from_str(&text)
        .map_err(|e| Error::from(format!("Could not parse '{}': {}", path.display(), e)))?;
    Ok(file
        .gene
        .into_iter()
        .map(|record| {
            (
                record.name,
                Pin {
                    source: record.source,
                    version: record.version,
                },
            )
        })
        .collect())
}

pub fn sync(root: &Path, graph: &Graph, locked: bool, ui: Ui) -> Result<(), Error> {
    let text = render(root, &graph.genes)?;
    let path = root.join(FILE_NAME);
    let current = match fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(Error::from(format!(
                "Could not read '{}': {}",
                path.display(),
                e
            )));
        }
    };
    if current.as_deref() == Some(text.as_str()) {
        return Ok(());
    }
    if locked {
        return Err(Error::with_help(
            format!("{} needs to be updated but --locked was passed.", FILE_NAME),
            "run again without --locked to update it",
        ));
    }
    ui.status(
        "Locking",
        &format!("{} gene(s) in {}", graph.genes.len(), FILE_NAME),
    );
    fs::write(&path, text)
        .map_err(|e| Error::from(format!("Could not write '{}': {}", path.display(), e)))
}

fn render(root: &Path, genes: &[Gene]) -> Result<String, Error> {
    let root = fs::canonicalize(root)
        .map_err(|e| Error::from(format!("Could not resolve '{}': {}", root.display(), e)))?;
    let gene = genes
        .iter()
        .map(|gene| {
            Ok(Entry {
                name: gene.name.clone(),
                source: source(&root, gene),
                version: gene.version.clone(),
                checksum: match &gene.source {
                    Source::Registry { checksum, .. } => Some(checksum.clone()),
                    _ => None,
                },
                sha256: digest(&gene.dir)?,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    toml::to_string(&Lock { gene })
        .map_err(|e| Error::from(format!("Could not render {}: {}", FILE_NAME, e)))
}

fn source(root: &Path, gene: &Gene) -> String {
    match &gene.source {
        Source::Path => slash(&relative(root, &gene.dir)),
        Source::Git { url, sha } => format!("git+{}#{}", redact(url), sha),
        Source::Registry { index, .. } => format!("registry+{}", index),
    }
}

pub fn digest(dir: &Path) -> Result<String, Error> {
    let src = dir.join("src");
    let mut files = Vec::new();
    collect_nk(&src, &mut files)
        .map_err(|e| Error::from(format!("Could not read '{}': {}", src.display(), e)))?;
    files.sort();

    let mut hasher = Sha256::new();
    for file in &files {
        let rel = file.strip_prefix(dir).unwrap_or(file);
        hasher.update(slash(rel).as_bytes());
        hasher.update([0]);
        let bytes = fs::read(file)
            .map_err(|e| Error::from(format!("Could not read '{}': {}", file.display(), e)))?;
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{:02x}", byte))
        .collect())
}

fn collect_nk(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_nk(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "nk") {
            out.push(path);
        }
    }
    Ok(())
}

fn relative(from: &Path, to: &Path) -> PathBuf {
    let from: Vec<Component> = from.components().collect();
    let to: Vec<Component> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    for part in &to[common..] {
        out.push(part);
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

fn slash(path: &Path) -> String {
    path.components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::temp_dir;
    use std::collections::BTreeMap;

    fn gene(base: &Path, name: &str, files: &[(&str, &str)]) -> io::Result<Gene> {
        let dir = base.join(name);
        for (path, text) in files {
            let full = dir.join(path);
            fs::create_dir_all(full.parent().unwrap_or(&dir))?;
            fs::write(full, text)?;
        }
        Ok(Gene {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            root: dir.join("src/lib.nk"),
            deps: Vec::new(),
            lints: BTreeMap::new(),
            source: Source::Path,
            dir,
        })
    }

    #[test]
    fn git_gene_records_url_and_resolved_sha() -> io::Result<()> {
        let base = temp_dir("git_source")?;
        let root = base.join("root");
        fs::create_dir_all(root.join("src"))?;
        let mut json = gene(&base, "json", &[("src/a.nk", "x")])?;
        json.source = Source::Git {
            url: "https://example.com/json.git".to_string(),
            sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
        };
        let text = render(&root, &[json]).map_err(io::Error::other);
        fs::remove_dir_all(&base)?;
        assert!(text?.contains(
            "source = \"git+https://example.com/json.git#0123456789abcdef0123456789abcdef01234567\""
        ));
        Ok(())
    }

    fn graph(genes: Vec<Gene>) -> Graph {
        Graph {
            deps: Vec::new(),
            genes,
        }
    }

    #[test]
    fn digest_ignores_files_outside_src_and_non_nk() -> io::Result<()> {
        let base = temp_dir("digest_scope")?;
        let plain = gene(&base, "plain", &[("src/a.nk", "x")])?;
        let noisy = gene(
            &base,
            "noisy",
            &[
                ("src/a.nk", "x"),
                ("src/notes.txt", "hi"),
                ("README.md", "hi"),
            ],
        )?;
        let (a, b) = (digest(&plain.dir), digest(&noisy.dir));
        fs::remove_dir_all(&base)?;
        assert_eq!(a.map_err(io::Error::other)?, b.map_err(io::Error::other)?);
        Ok(())
    }

    #[test]
    fn digest_changes_with_contents_and_with_file_name() -> io::Result<()> {
        let base = temp_dir("digest_change")?;
        let first = gene(&base, "first", &[("src/a.nk", "x")])?;
        let edited = gene(&base, "edited", &[("src/a.nk", "y")])?;
        let renamed = gene(&base, "renamed", &[("src/b.nk", "x")])?;
        let original = digest(&first.dir);
        let edit = digest(&edited.dir);
        let rename = digest(&renamed.dir);
        fs::remove_dir_all(&base)?;
        let original = original.map_err(io::Error::other)?;
        assert_ne!(original, edit.map_err(io::Error::other)?);
        assert_ne!(original, rename.map_err(io::Error::other)?);
        Ok(())
    }

    #[test]
    fn digest_does_not_depend_on_directory_creation_order() -> io::Result<()> {
        let base = temp_dir("digest_order")?;
        let one = gene(&base, "one", &[("src/a.nk", "1"), ("src/b/c.nk", "2")])?;
        let two = gene(&base, "two", &[("src/b/c.nk", "2"), ("src/a.nk", "1")])?;
        let (a, b) = (digest(&one.dir), digest(&two.dir));
        fs::remove_dir_all(&base)?;
        assert_eq!(a.map_err(io::Error::other)?, b.map_err(io::Error::other)?);
        Ok(())
    }

    #[test]
    fn render_lists_genes_with_relative_sources() -> io::Result<()> {
        let base = temp_dir("render")?;
        let root = base.join("root");
        fs::create_dir_all(root.join("src"))?;
        let a = gene(&base, "a", &[("src/a.nk", "x")])?;
        let b = gene(&base, "b", &[("src/b.nk", "y")])?;
        let text = render(&root, &[a, b]).map_err(io::Error::other);
        fs::remove_dir_all(&base)?;
        let text = text?;
        assert!(text.contains("name = \"a\""), "{text}");
        assert!(text.contains("source = \"../a\""), "{text}");
        assert!(text.contains("source = \"../b\""), "{text}");
        assert_eq!(text.matches("[[gene]]").count(), 2, "{text}");
        Ok(())
    }

    #[test]
    fn sync_writes_then_leaves_current_lock_alone() -> io::Result<()> {
        let base = temp_dir("sync")?;
        let root = base.join("root");
        fs::create_dir_all(root.join("src"))?;
        let g = graph(vec![gene(&base, "a", &[("src/a.nk", "x")])?]);
        let first = sync(&root, &g, false, Ui::default());
        let written = fs::read_to_string(root.join(FILE_NAME))?;
        let again = sync(&root, &g, true, Ui::default());
        fs::remove_dir_all(&base)?;
        first.map_err(io::Error::other)?;
        again.map_err(io::Error::other)?;
        assert!(written.contains("sha256 = "), "{written}");
        Ok(())
    }

    #[test]
    fn sync_updates_changed_gene_when_not_locked() -> io::Result<()> {
        let base = temp_dir("update")?;
        let root = base.join("root");
        fs::create_dir_all(root.join("src"))?;
        let a = gene(&base, "a", &[("src/a.nk", "x")])?;
        let dir = a.dir.clone();
        sync(&root, &graph(vec![a]), false, Ui::default()).map_err(io::Error::other)?;
        let before = fs::read_to_string(root.join(FILE_NAME))?;

        fs::write(dir.join("src/a.nk"), "changed")?;
        let changed = graph(vec![gene(&base, "a", &[("src/a.nk", "changed")])?]);
        let result = sync(&root, &changed, false, Ui::default());
        let after = fs::read_to_string(root.join(FILE_NAME))?;
        fs::remove_dir_all(&base)?;
        result.map_err(io::Error::other)?;
        assert_ne!(before, after);
        Ok(())
    }

    #[test]
    fn locked_refuses_changed_lock_and_leaves_it_alone() -> io::Result<()> {
        let base = temp_dir("locked")?;
        let root = base.join("root");
        fs::create_dir_all(root.join("src"))?;
        fs::write(root.join(FILE_NAME), "stale\n")?;
        let g = graph(vec![gene(&base, "a", &[("src/a.nk", "x")])?]);
        let result = sync(&root, &g, true, Ui::default());
        let kept = fs::read_to_string(root.join(FILE_NAME))?;
        fs::remove_dir_all(&base)?;
        let err = result.unwrap_err();
        assert!(err.contains("--locked"), "{err}");
        assert_eq!(kept, "stale\n");
        Ok(())
    }

    #[test]
    fn locked_refuses_missing_lock() -> io::Result<()> {
        let base = temp_dir("locked_missing")?;
        let root = base.join("root");
        fs::create_dir_all(root.join("src"))?;
        let result = sync(&root, &graph(Vec::new()), true, Ui::default());
        let exists = root.join(FILE_NAME).exists();
        fs::remove_dir_all(&base)?;
        assert!(result.is_err());
        assert!(!exists);
        Ok(())
    }

    #[test]
    fn git_source_in_lock_drops_credentials() -> io::Result<()> {
        let base = temp_dir("git_redact_lock")?;
        let root = base.join("root");
        fs::create_dir_all(root.join("src"))?;
        let mut json = gene(&base, "json", &[("src/a.nk", "x")])?;
        json.source = Source::Git {
            url: "https://user:secret@example.com/json.git".to_string(),
            sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
        };
        let text = render(&root, &[json]).map_err(io::Error::other);
        fs::remove_dir_all(&base)?;
        let text = text?;
        assert!(
            text.contains("source = \"git+https://example.com/json.git#0123456789abcdef0123456789abcdef01234567\""),
            "{text}"
        );
        assert!(!text.contains("secret"), "{text}");
        Ok(())
    }

    #[test]
    fn relative_climbs_out_of_root() {
        assert_eq!(
            relative(Path::new("/p/root"), Path::new("/p/json")),
            PathBuf::from("../json")
        );
        assert_eq!(
            relative(Path::new("/p/root"), Path::new("/q/x/y")),
            PathBuf::from("../../q/x/y")
        );
    }
}
