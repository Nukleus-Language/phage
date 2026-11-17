use crate::{
    deps::{Gene, Graph, Source},
    error::Error,
    git::redact,
    lock,
};
use sha2::{Digest, Sha256};
use std::{
    env,
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::UNIX_EPOCH,
};

const DIR: &str = ".fingerprint";

/// Stored next to an output as `.fingerprint/<file name>`. The output is fresh when the stamp
/// holds the value computed now and the output file still exists.
pub struct Stamp {
    path: PathBuf,
    artifact: PathBuf,
    value: String,
}

impl Stamp {
    pub fn new(artifact: &Path, sources: &str, args: &[OsString]) -> Stamp {
        let mut hasher = Sha256::new();
        feed(&mut hasher, "sources", sources.as_bytes());
        for arg in args {
            feed(&mut hasher, "arg", arg.as_encoded_bytes());
        }
        let parent = artifact.parent().unwrap_or(Path::new("."));
        Stamp {
            path: parent
                .join(DIR)
                .join(artifact.file_name().unwrap_or_default()),
            artifact: artifact.to_path_buf(),
            value: hex(&hasher.finalize()),
        }
    }

    pub fn is_fresh(&self) -> bool {
        self.artifact.is_file()
            && fs::read_to_string(&self.path).is_ok_and(|stored| stored == self.value)
    }

    pub fn clear(&self) -> Result<(), Error> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::from(format!(
                "Could not remove '{}': {}",
                self.path.display(),
                e
            ))),
        }
    }

    pub fn write(&self) -> Result<(), Error> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)
                .map_err(|e| Error::from(format!("Could not create '{}': {}", dir.display(), e)))?;
        }
        fs::write(&self.path, &self.value)
            .map_err(|e| Error::from(format!("Could not write '{}': {}", self.path.display(), e)))
    }
}

/// Hash of every input nukleus sees for one invocation: the nukleus binary, the root's
/// sources, and every gene's name, version, source and sources.
pub fn sources(nukleus: &OsStr, root: &Path, graph: &Graph) -> Result<String, Error> {
    let mut hasher = Sha256::new();
    feed(&mut hasher, "nukleus", nukleus_identity(nukleus).as_bytes());
    feed(&mut hasher, "root", lock::digest(root)?.as_bytes());
    for gene in &graph.genes {
        let line = format!("{} {} {}", gene.name, gene.version, source_text(gene));
        feed(&mut hasher, "gene", line.as_bytes());
        feed(&mut hasher, "gene-src", lock::digest(&gene.dir)?.as_bytes());
    }
    Ok(hex(&hasher.finalize()))
}

fn source_text(gene: &Gene) -> String {
    match &gene.source {
        Source::Path => gene.dir.display().to_string(),
        Source::Git { url, sha } => format!("git+{}#{}", redact(url), sha),
    }
}

fn nukleus_identity(nukleus: &OsStr) -> String {
    let missing = format!("{}:missing", nukleus.to_string_lossy());
    let Some(path) = find_program(nukleus) else {
        return missing;
    };
    let Ok(meta) = fs::metadata(&path) else {
        return missing;
    };
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |time| time.as_nanos());
    let version = Command::new(&path)
        .arg("--version")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default();
    format!(
        "{}\n{}\n{}\n{}",
        path.display(),
        meta.len(),
        modified,
        version
    )
}

fn find_program(program: &OsStr) -> Option<PathBuf> {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return path.is_file().then(|| path.to_path_buf());
    }
    env::split_paths(&env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

fn feed(hasher: &mut Sha256, label: &str, bytes: &[u8]) {
    hasher.update(label.as_bytes());
    hasher.update([0]);
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{:02x}", byte)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::temp_dir;
    use std::collections::BTreeMap;

    fn gene(base: &Path, name: &str, text: &str) -> io::Result<Gene> {
        let dir = base.join(name);
        fs::create_dir_all(dir.join("src"))?;
        fs::write(dir.join("src/lib.nk"), text)?;
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

    fn graph(genes: Vec<Gene>) -> Graph {
        Graph {
            deps: Vec::new(),
            genes,
        }
    }

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn stamp_is_fresh_only_while_artifact_and_value_match() -> io::Result<()> {
        let base = temp_dir("stamp_fresh")?;
        let artifact = base.join("demo");
        fs::write(&artifact, "bin")?;
        let stamp = Stamp::new(&artifact, "s1", &args(&["-o"]));
        let before = stamp.is_fresh();
        stamp.write().map_err(io::Error::other)?;
        let after = stamp.is_fresh();
        let edited = Stamp::new(&artifact, "s2", &args(&["-o"])).is_fresh();
        fs::remove_file(&artifact)?;
        let removed = stamp.is_fresh();
        fs::remove_dir_all(&base)?;
        assert!(!before);
        assert!(after);
        assert!(!edited);
        assert!(!removed);
        Ok(())
    }

    #[test]
    fn stamp_value_changes_with_arguments() {
        let artifact = Path::new("/p/target/debug/demo");
        let plain = Stamp::new(artifact, "s", &args(&["-o"]));
        let lint = Stamp::new(artifact, "s", &args(&["-o", "--lint", "x=deny"]));
        assert_ne!(plain.value, lint.value);
        assert_eq!(plain.path, Path::new("/p/target/debug/.fingerprint/demo"));
    }

    #[test]
    fn sources_change_when_a_gene_file_changes() -> io::Result<()> {
        let base = temp_dir("sources_gene")?;
        let root = base.join("root");
        fs::create_dir_all(root.join("src"))?;
        fs::write(root.join("src/main.nk"), "")?;
        let nukleus = Path::new("/no/such/nukleus");
        let before = sources(
            nukleus.as_os_str(),
            &root,
            &graph(vec![gene(&base, "a", "x")?]),
        );
        let after = sources(
            nukleus.as_os_str(),
            &root,
            &graph(vec![gene(&base, "a", "y")?]),
        );
        fs::remove_dir_all(&base)?;
        assert_ne!(
            before.map_err(io::Error::other)?,
            after.map_err(io::Error::other)?
        );
        Ok(())
    }

    #[test]
    fn sources_change_when_a_root_file_changes() -> io::Result<()> {
        let base = temp_dir("sources_root")?;
        let root = base.join("root");
        fs::create_dir_all(root.join("src"))?;
        fs::write(root.join("src/main.nk"), "x")?;
        let nukleus = Path::new("/no/such/nukleus");
        let first = sources(nukleus.as_os_str(), &root, &graph(Vec::new()));
        fs::write(root.join("src/main.nk"), "y")?;
        let second = sources(nukleus.as_os_str(), &root, &graph(Vec::new()));
        fs::remove_dir_all(&base)?;
        assert_ne!(
            first.map_err(io::Error::other)?,
            second.map_err(io::Error::other)?
        );
        Ok(())
    }

    #[test]
    fn nukleus_binary_mtime_is_part_of_sources() -> io::Result<()> {
        let base = temp_dir("sources_mtime")?;
        let root = base.join("root");
        fs::create_dir_all(root.join("src"))?;
        fs::write(root.join("src/main.nk"), "")?;
        let binary = base.join("fake-nukleus");
        fs::write(&binary, "not a program")?;
        let before = sources(binary.as_os_str(), &root, &graph(Vec::new()));
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
        fs::OpenOptions::new()
            .write(true)
            .open(&binary)?
            .set_modified(later)?;
        let after = sources(binary.as_os_str(), &root, &graph(Vec::new()));
        fs::remove_dir_all(&base)?;
        assert_ne!(
            before.map_err(io::Error::other)?,
            after.map_err(io::Error::other)?
        );
        Ok(())
    }
}
