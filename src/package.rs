use crate::{
    error::Error,
    manifest::{Dependency, FILE_NAME, Manifest},
    registry::{self, IndexDep, IndexLine},
    ui::Ui,
    workspace::Workspace,
};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// A packaged gene: where the tarball is, its checksum, and the index line that describes it.
pub struct Built {
    pub path: PathBuf,
    pub sha256: String,
    pub line: IndexLine,
}

/// Writes `Phage.toml` and `src/**` into a gzipped tar with fixed metadata, so the same sources
/// always produce the same bytes.
pub fn build_archive(dir: &Path, manifest: &Manifest, out: &Path) -> Result<Built, Error> {
    let name = manifest.name();
    let version = manifest.version();
    registry::validate_name(name)?;
    let deps = index_deps(manifest)?;

    let mut files = vec![(FILE_NAME.to_string(), dir.join(FILE_NAME))];
    collect(&dir.join("src"), "src", &mut files)?;
    files.sort();

    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| Error::from(format!("Could not create '{}': {}", parent.display(), e)))?;
    }
    let file = fs::File::create(out)
        .map_err(|e| Error::from(format!("Could not create '{}': {}", out.display(), e)))?;
    let gz = flate2::GzBuilder::new()
        .mtime(0)
        .write(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(gz);
    for (rel, path) in &files {
        let data = fs::read(path)
            .map_err(|e| Error::from(format!("Could not read '{}': {}", path.display(), e)))?;
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header
            .set_username("")
            .and_then(|_| header.set_groupname(""))
            .map_err(|e| Error::from(format!("tar header: {}", e)))?;
        header.set_entry_type(tar::EntryType::Regular);
        builder
            .append_data(&mut header, rel, &data[..])
            .map_err(|e| Error::from(format!("tar: {}", e)))?;
    }
    let gz = builder
        .into_inner()
        .map_err(|e| Error::from(format!("tar: {}", e)))?;
    gz.finish()
        .map_err(|e| Error::from(format!("gzip: {}", e)))?;

    let bytes = fs::read(out)
        .map_err(|e| Error::from(format!("Could not read '{}': {}", out.display(), e)))?;
    let sha256 = registry::sha256_hex(&bytes);
    Ok(Built {
        path: out.to_path_buf(),
        sha256: sha256.clone(),
        line: IndexLine {
            name: name.to_string(),
            vers: version.to_string(),
            deps,
            cksum: sha256,
            yanked: false,
        },
    })
}

fn index_deps(manifest: &Manifest) -> Result<Vec<IndexDep>, Error> {
    let mut deps = Vec::new();
    for (name, dep) in &manifest.dependencies {
        match dep {
            Dependency::Req(req) => deps.push(IndexDep {
                name: name.clone(),
                req: req.clone(),
            }),
            _ => {
                return Err(Error::with_help(
                    format!(
                        "cannot publish: '{}' is a path, git or gene dependency",
                        name
                    ),
                    "published genes may depend only on registry versions (name = \"^1.2\")",
                ));
            }
        }
    }
    Ok(deps)
}

fn collect(dir: &Path, rel: &str, out: &mut Vec<(String, PathBuf)>) -> Result<(), Error> {
    let entries = fs::read_dir(dir)
        .map_err(|e| Error::from(format!("Could not read '{}': {}", dir.display(), e)))?;
    for entry in entries {
        let entry =
            entry.map_err(|e| Error::from(format!("Could not read '{}': {}", dir.display(), e)))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let child = format!("{}/{}", rel, name);
        if path.is_dir() {
            collect(&path, &child, out)?;
        } else {
            out.push((child, path));
        }
    }
    Ok(())
}

/// `phage package`: builds the tarball for the one selected package and prints its line.
pub fn package(names: &[String], ui: Ui, publish: bool) -> Result<(), Error> {
    let cwd = std::env::current_dir()
        .map_err(|e| Error::from(format!("Could not read the current directory: {}", e)))?;
    let ws = Workspace::discover(&cwd)?;
    let package = ws.only(names, if publish { "publish" } else { "package" })?;
    let out = ws.root.join("target").join("package").join(format!(
        "{}-{}.tar.gz",
        package.manifest.name(),
        package.manifest.version()
    ));
    let built = build_archive(&package.dir, &package.manifest, &out)?;
    ui.status(
        "Packaged",
        &format!(
            "{} v{} ({})",
            built.line.name,
            built.line.vers,
            built.path.display()
        ),
    );
    let line = serde_json::to_string(&built.line)
        .map_err(|e| Error::from(format!("Could not render index line: {}", e)))?;
    println!("sha256 {}", built.sha256);
    println!("index line: {}", line);
    if publish {
        let entry = registry::index_path(&built.line.name)?;
        println!();
        println!("To publish {} v{}:", built.line.name, built.line.vers);
        println!(
            "  1. Upload {} to a Nukleus-Language/genes-store release tagged {}-{}.",
            built.path.display(),
            built.line.name,
            built.line.vers
        );
        println!(
            "  2. Add the index line above as the last line of {} in genes-index.",
            entry
        );
        println!("  3. Open a pull request to Nukleus-Language/genes-index.");
        println!("phage does not upload or open the pull request.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{manifest, testutil::temp_dir};
    use std::io;

    fn gene(base: &Path, name: &str, version: &str, deps: &str) -> io::Result<PathBuf> {
        let dir = base.join(name);
        fs::create_dir_all(dir.join("src"))?;
        fs::write(
            dir.join(FILE_NAME),
            format!(
                "[project]\nname = \"{name}\"\nversion = \"{version}\"\n\n[dependencies]\n{deps}"
            ),
        )?;
        fs::write(
            dir.join("src/lib.nk"),
            format!("fn {name}() -> i32 {{ return 0; }}\n"),
        )?;
        Ok(dir)
    }

    #[test]
    fn same_sources_give_same_bytes() -> io::Result<()> {
        let base = temp_dir("pkg_det")?;
        let dir = gene(&base, "json", "1.0.0", "")?;
        let manifest = manifest::load(&dir.join(FILE_NAME)).map_err(io::Error::other)?;
        let one =
            build_archive(&dir, &manifest, &base.join("one.tar.gz")).map_err(io::Error::other);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fs::write(dir.join("src/lib.nk"), "fn json() -> i32 { return 0; }\n")?;
        let two =
            build_archive(&dir, &manifest, &base.join("two.tar.gz")).map_err(io::Error::other);
        let same = fs::read(base.join("one.tar.gz"))? == fs::read(base.join("two.tar.gz"))?;
        fs::remove_dir_all(&base)?;
        assert_eq!(one?.sha256, two?.sha256);
        assert!(same);
        Ok(())
    }

    #[test]
    fn index_line_lists_registry_deps_and_refuses_path_deps() -> io::Result<()> {
        let base = temp_dir("pkg_deps")?;
        let ok = gene(&base, "app", "0.1.0", "json = \"^1.2\"\n")?;
        let ok_manifest = manifest::load(&ok.join(FILE_NAME)).map_err(io::Error::other)?;
        let built =
            build_archive(&ok, &ok_manifest, &base.join("ok.tar.gz")).map_err(io::Error::other)?;

        let bad = gene(&base, "bad", "0.1.0", "local = { path = \"../local\" }\n")?;
        let bad_manifest = manifest::load(&bad.join(FILE_NAME)).map_err(io::Error::other)?;
        let refused = build_archive(&bad, &bad_manifest, &base.join("bad.tar.gz")).err();
        fs::remove_dir_all(&base)?;
        assert_eq!(
            built.line.deps,
            vec![IndexDep {
                name: "json".into(),
                req: "^1.2".into()
            }]
        );
        assert_eq!(built.line.cksum, built.sha256);
        assert!(refused.is_some_and(|e| e.contains("cannot publish")));
        Ok(())
    }
}
