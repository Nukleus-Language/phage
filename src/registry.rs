use crate::{error::Error, git::redact, lock::Pin, manifest, ui::Ui};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    env, fs, io,
    path::{Path, PathBuf},
};

pub const DEFAULT_INDEX: &str = "https://nukleus-language.github.io/genes-index/";
const RESERVED: &[&str] = &["std", "core", "nukleus", "phage"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexLine {
    pub name: String,
    pub vers: String,
    #[serde(default)]
    pub deps: Vec<IndexDep>,
    pub cksum: String,
    #[serde(default)]
    pub yanked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexDep {
    pub name: String,
    pub req: String,
}

#[derive(Debug, Deserialize)]
struct Config {
    dl: String,
}

/// A version chosen from the index, ready to install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    pub name: String,
    pub version: String,
    pub cksum: String,
    pub url: String,
}

pub struct Registry {
    base: String,
    home: PathBuf,
    offline: bool,
    pub ui: Ui,
    pins: BTreeMap<String, Pin>,
    memo: RefCell<BTreeMap<String, Vec<IndexLine>>>,
}

impl Registry {
    pub fn new(home: PathBuf, offline: bool, base: &str) -> Registry {
        Registry {
            base: base.to_string(),
            home,
            offline,
            ui: Ui::default(),
            pins: BTreeMap::new(),
            memo: RefCell::new(BTreeMap::new()),
        }
    }

    pub fn index_from_env() -> String {
        env::var("PHAGE_INDEX").unwrap_or_else(|_| DEFAULT_INDEX.to_string())
    }

    pub fn with_pins(mut self, pins: BTreeMap<String, Pin>) -> Registry {
        self.pins = pins;
        self
    }

    /// The label recorded in Phage.lock as `source = "registry+<label>"`.
    pub fn label(&self) -> String {
        redact(&self.base)
    }

    pub fn pick(&self, name: &str, req: &VersionReq) -> Result<Pick, Error> {
        let lines = self.index_lines(name)?;
        let source = format!("registry+{}", self.label());
        let pinned = self
            .pins
            .get(name)
            .filter(|pin| pin.source == source)
            .map(|pin| pin.version.as_str());
        let line = pick_line(&lines, req, pinned).ok_or_else(|| {
            let mut available: Vec<&str> = lines
                .iter()
                .filter(|line| !line.yanked)
                .map(|line| line.vers.as_str())
                .collect();
            available.sort();
            let help = if available.is_empty() {
                format!("'{}' has no versions that are not yanked", name)
            } else {
                format!("available: {}", available.join(", "))
            };
            Error::with_help(format!("no version of '{}' matches {}", name, req), help)
        })?;
        let config = self.config()?;
        Ok(Pick {
            name: name.to_string(),
            version: line.vers.clone(),
            cksum: line.cksum.clone(),
            url: download_url(&config.dl, name, &line.vers),
        })
    }

    /// Makes sure `<home>/registry/src/<name>-<version>` holds the verified package.
    pub fn install(&self, pick: &Pick) -> Result<PathBuf, Error> {
        let src = self.home.join("registry").join("src");
        let dest = src.join(format!("{}-{}", pick.name, pick.version));
        let marker = src.join(format!("{}-{}.cksum", pick.name, pick.version));
        if dest.is_dir() && fs::read_to_string(&marker).is_ok_and(|text| text == pick.cksum) {
            return Ok(dest);
        }
        if self.offline {
            return Err(Error::with_help(
                format!(
                    "--offline: '{}' {} is not in the cache",
                    pick.name, pick.version
                ),
                "run once without --offline to download it",
            ));
        }
        self.ui.status(
            "Downloading",
            &format!(
                "{} v{} (registry+{})",
                pick.name,
                pick.version,
                self.label()
            ),
        );
        let bytes = fetch_optional(&pick.url)?.ok_or_else(|| {
            Error::from(format!(
                "download of {} v{} failed: nothing at {}",
                pick.name,
                pick.version,
                redact(&pick.url)
            ))
        })?;
        let actual = sha256_hex(&bytes);
        if !actual.eq_ignore_ascii_case(&pick.cksum) {
            return Err(Error::with_help(
                format!(
                    "checksum mismatch for {} v{}: index says {}, download is {}",
                    pick.name, pick.version, pick.cksum, actual
                ),
                "the tarball does not match the index; the download was not unpacked",
            ));
        }

        let tmp = self.home.join("registry").join("tmp").join(format!(
            "{}-{}-{}",
            pick.name,
            pick.version,
            std::process::id()
        ));
        if tmp.exists() {
            fs::remove_dir_all(&tmp).map_err(|e| io_error(&tmp, e))?;
        }
        if let Err(err) = unpack(&bytes, &tmp) {
            let _ = fs::remove_dir_all(&tmp);
            return Err(err);
        }
        let manifest = manifest::load(&tmp.join(manifest::FILE_NAME));
        let matches = manifest
            .as_ref()
            .map(|m| m.name() == pick.name && m.version() == pick.version);
        if matches.is_err() || matches == Ok(false) {
            let _ = fs::remove_dir_all(&tmp);
            return Err(Error::from(format!(
                "archive for {} v{} does not contain a matching {}",
                pick.name,
                pick.version,
                manifest::FILE_NAME
            )));
        }
        if dest.exists() {
            fs::remove_dir_all(&dest).map_err(|e| io_error(&dest, e))?;
        }
        fs::create_dir_all(&src).map_err(|e| io_error(&src, e))?;
        fs::rename(&tmp, &dest).map_err(|e| io_error(&dest, e))?;
        fs::write(&marker, &pick.cksum).map_err(|e| io_error(&marker, e))?;
        Ok(dest)
    }

    /// Names in the index's `names.txt`, filtered by a case-insensitive substring match.
    pub fn search(&self, query: &str) -> Result<Vec<String>, Error> {
        self.config()?;
        let names = self.index_file("names.txt")?.ok_or_else(|| {
            Error::with_help(
                format!("the index at {} has no names.txt", self.label()),
                "search needs a names list in the index",
            )
        })?;
        let query = query.to_lowercase();
        let text = String::from_utf8_lossy(&names);
        Ok(text
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty() && name.to_lowercase().contains(&query))
            .map(str::to_string)
            .collect())
    }

    fn index_lines(&self, name: &str) -> Result<Vec<IndexLine>, Error> {
        validate_name(name)?;
        if let Some(lines) = self.memo.borrow().get(name) {
            return Ok(lines.clone());
        }
        let rel = index_path(name)?;
        let bytes = match self.index_file(&rel)? {
            Some(bytes) => bytes,
            None if self.offline => {
                return Err(Error::with_help(
                    format!("--offline: no cached index entry for '{}'", name),
                    "run once without --offline to fill the cache",
                ));
            }
            None => {
                return Err(Error::with_help(
                    format!("no gene named '{}' in the index", name),
                    format!("check the name; index: {}", self.label()),
                ));
            }
        };
        let lines = parse_lines(&bytes, &rel)?;
        self.memo
            .borrow_mut()
            .insert(name.to_string(), lines.clone());
        Ok(lines)
    }

    fn config(&self) -> Result<Config, Error> {
        let bytes = self.index_file("config.json")?.ok_or_else(|| {
            Error::with_help(
                format!("the index at {} has no config.json", self.label()),
                "the index must be in sparse layout (config.json plus ab/cd/name files)",
            )
        })?;
        serde_json::from_slice(&bytes)
            .map_err(|e| Error::from(format!("bad config.json in index {}: {}", self.label(), e)))
    }

    /// Fetches one index-relative file, through the cache under `<home>/registry/index`.
    fn index_file(&self, rel: &str) -> Result<Option<Vec<u8>>, Error> {
        let cache = self
            .home
            .join("registry")
            .join("index")
            .join(cache_key(&self.base))
            .join(rel);
        if self.offline {
            return match fs::read(&cache) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(io_error(&cache, e)),
            };
        }
        let url = join_url(&self.base, rel);
        match fetch_optional(&url)? {
            Some(bytes) => {
                if let Some(dir) = cache.parent() {
                    fs::create_dir_all(dir).map_err(|e| io_error(dir, e))?;
                }
                fs::write(&cache, &bytes).map_err(|e| io_error(&cache, e))?;
                Ok(Some(bytes))
            }
            None => Ok(None),
        }
    }
}

fn io_error(path: &Path, e: io::Error) -> Error {
    Error::from(format!("'{}': {}", path.display(), e))
}

pub fn validate_name(name: &str) -> Result<(), Error> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !ok {
        return Err(Error::with_help(
            format!("'{}' is not a valid gene name", name),
            "use 1 to 64 characters from a-z, 0-9, '-' and '_'",
        ));
    }
    if RESERVED.contains(&name) {
        return Err(Error::from(format!("'{}' is a reserved name", name)));
    }
    Ok(())
}

/// Sparse index layout, as cargo uses it: `1/x`, `2/xy`, `3/x/xyz`, `ab/cd/abcd...`.
pub fn index_path(name: &str) -> Result<String, Error> {
    validate_name(name)?;
    Ok(match name.len() {
        1 => format!("1/{}", name),
        2 => format!("2/{}", name),
        3 => format!("3/{}/{}", &name[..1], name),
        _ => format!("{}/{}/{}", &name[..2], &name[2..4], name),
    })
}

pub fn parse_lines(bytes: &[u8], origin: &str) -> Result<Vec<IndexLine>, Error> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| Error::from(format!("index file {} is not UTF-8", origin)))?;
    let mut lines = Vec::new();
    for (number, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let parsed: IndexLine = serde_json::from_str(line)
            .map_err(|e| Error::from(format!("bad index line {}:{}: {}", origin, number + 1, e)))?;
        Version::parse(&parsed.vers).map_err(|e| {
            Error::from(format!(
                "bad version '{}' in {}:{}: {}",
                parsed.vers,
                origin,
                number + 1,
                e
            ))
        })?;
        if parsed.cksum.len() != 64 || !parsed.cksum.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::from(format!(
                "bad cksum for {} {} in {}:{}: expected 64 hex digits",
                parsed.name,
                parsed.vers,
                origin,
                number + 1
            )));
        }
        lines.push(parsed);
    }
    Ok(lines)
}

/// Highest non-yanked version that matches `req`; a pinned version wins when it still matches.
pub fn pick_line<'a>(
    lines: &'a [IndexLine],
    req: &VersionReq,
    pinned: Option<&str>,
) -> Option<&'a IndexLine> {
    let candidates: Vec<(&IndexLine, Version)> = lines
        .iter()
        .filter(|line| !line.yanked)
        .filter_map(|line| {
            Version::parse(&line.vers)
                .ok()
                .map(|version| (line, version))
        })
        .filter(|(_, version)| req.matches(version))
        .collect();
    if let Some(pinned) = pinned {
        if let Some((line, _)) = candidates.iter().find(|(line, _)| line.vers == pinned) {
            return Some(line);
        }
    }
    candidates
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1))
        .map(|(line, _)| line)
}

pub fn download_url(template: &str, name: &str, version: &str) -> String {
    template
        .replace("{name}", name)
        .replace("{version}", version)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{:02x}", byte))
        .collect()
}

fn cache_key(base: &str) -> String {
    sha256_hex(base.as_bytes())[..16].to_string()
}

fn join_url(base: &str, rel: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), rel)
}

/// Fetches a `file://` path, a plain path, or an http(s) URL. `None` means not found.
pub fn fetch_optional(url: &str) -> Result<Option<Vec<u8>>, Error> {
    if url.starts_with("http://") || url.starts_with("https://") {
        return match ureq::get(url).call() {
            Ok(mut response) => response
                .body_mut()
                .read_to_vec()
                .map(Some)
                .map_err(|e| Error::from(format!("could not read {}: {}", redact(url), e))),
            Err(ureq::Error::StatusCode(404)) => Ok(None),
            Err(e) => Err(Error::from(format!(
                "could not fetch {}: {}",
                redact(url),
                e
            ))),
        };
    }
    let path = url.strip_prefix("file://").unwrap_or(url);
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error(Path::new(path), e)),
    }
}

/// Unpacks a gzipped tarball of regular files and directories into `dest`.
pub fn unpack(bytes: &[u8], dest: &Path) -> Result<(), Error> {
    fs::create_dir_all(dest).map_err(|e| io_error(dest, e))?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let entries = archive
        .entries()
        .map_err(|e| Error::from(format!("bad tarball: {}", e)))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| Error::from(format!("bad tarball: {}", e)))?;
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            return Err(Error::from(
                "tarball contains a link or special file; packages may only hold files and directories",
            ));
        }
        let inside = entry
            .unpack_in(dest)
            .map_err(|e| Error::from(format!("bad tarball: {}", e)))?;
        if !inside {
            return Err(Error::from("tarball entry escapes the package directory"));
        }
    }
    Ok(())
}

/// Checks a local checkout of an index: layout, JSON, versions, names, deps, and tarball checksums.
pub fn check_index(dir: &Path, download: bool) -> Result<usize, Error> {
    let config_bytes = fs::read(dir.join("config.json"))
        .map_err(|_| Error::from("index has no config.json at its root"))?;
    let config: Config = serde_json::from_slice(&config_bytes)
        .map_err(|e| Error::from(format!("bad config.json: {}", e)))?;

    let mut files = Vec::new();
    collect_index_files(dir, dir, &mut files)?;
    let mut problems: Vec<String> = Vec::new();
    let mut known: BTreeMap<String, Vec<IndexLine>> = BTreeMap::new();
    let mut checked = 0;

    for (rel, path) in &files {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Err(e) = validate_name(name) {
            problems.push(format!("{}: {}", rel, e));
            continue;
        }
        match index_path(name) {
            Ok(expected) if expected == *rel => {}
            _ => {
                problems.push(format!(
                    "{}: file must live at {}",
                    rel,
                    index_path(name).unwrap_or_default()
                ));
                continue;
            }
        }
        let bytes = fs::read(path).map_err(|e| io_error(path, e))?;
        match parse_lines(&bytes, rel) {
            Ok(lines) => {
                if lines.is_empty() {
                    problems.push(format!("{}: no version lines", rel));
                }
                known.insert(name.to_string(), lines);
            }
            Err(e) => problems.push(e.msg),
        }
    }

    for (name, lines) in &known {
        let mut seen = std::collections::BTreeSet::new();
        for line in lines {
            checked += 1;
            if line.name != *name {
                problems.push(format!("{}: line says name '{}'", name, line.name));
            }
            if !seen.insert(line.vers.clone()) {
                problems.push(format!("{} {}: version listed twice", name, line.vers));
            }
            for dep in &line.deps {
                if VersionReq::parse(&dep.req).is_err() {
                    problems.push(format!(
                        "{} {}: bad req '{}' for {}",
                        name, line.vers, dep.req, dep.name
                    ));
                }
                if !known.contains_key(&dep.name) {
                    problems.push(format!(
                        "{} {}: dependency '{}' is not in the index",
                        name, line.vers, dep.name
                    ));
                }
            }
            if download {
                let url = download_url(&config.dl, name, &line.vers);
                match fetch_optional(&url) {
                    Ok(Some(bytes)) => {
                        let actual = sha256_hex(&bytes);
                        if !actual.eq_ignore_ascii_case(&line.cksum) {
                            problems.push(format!(
                                "{} {}: cksum {} does not match the tarball ({})",
                                name, line.vers, line.cksum, actual
                            ));
                        }
                    }
                    Ok(None) => {
                        problems.push(format!("{} {}: no tarball at {}", name, line.vers, url))
                    }
                    Err(e) => problems.push(format!("{} {}: {}", name, line.vers, e.msg)),
                }
            }
        }
    }

    if problems.is_empty() {
        Ok(checked)
    } else {
        Err(Error::from(format!(
            "index check found {} problem(s):\n  {}",
            problems.len(),
            problems.join("\n  ")
        )))
    }
}

fn collect_index_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), Error> {
    let entries = fs::read_dir(dir).map_err(|e| io_error(dir, e))?;
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for path in paths {
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if file_name.starts_with('.')
            || matches!(file_name, "README.md" | "LICENSE" | "config.json")
        {
            continue;
        }
        if path.is_dir() {
            collect_index_files(root, &path, out)?;
        } else {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            out.push((rel, path));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::temp_dir;

    fn line(vers: &str, yanked: bool) -> IndexLine {
        IndexLine {
            name: "json".to_string(),
            vers: vers.to_string(),
            deps: Vec::new(),
            cksum: "0".repeat(64),
            yanked,
        }
    }

    #[test]
    fn sparse_layout_matches_cargo() -> Result<(), Error> {
        assert_eq!(index_path("a")?, "1/a");
        assert_eq!(index_path("ab")?, "2/ab");
        assert_eq!(index_path("abc")?, "3/a/abc");
        assert_eq!(index_path("json")?, "js/on/json");
        assert_eq!(index_path("my-gene_2")?, "my/-g/my-gene_2");
        Ok(())
    }

    #[test]
    fn names_follow_the_rules() {
        assert!(validate_name("json").is_ok());
        assert!(validate_name("Json").is_err());
        assert!(validate_name("a.b").is_err());
        assert!(validate_name("").is_err());
        assert!(validate_name(&"a".repeat(65)).is_err());
        let reserved = validate_name("std").unwrap_err();
        assert!(reserved.contains("reserved"), "{reserved}");
    }

    #[test]
    fn highest_matching_version_wins() -> Result<(), semver::Error> {
        let lines = vec![
            line("1.0.0", false),
            line("1.2.0", false),
            line("2.0.0", false),
        ];
        let req = VersionReq::parse("^1.0")?;
        assert_eq!(
            pick_line(&lines, &req, None).map(|l| l.vers.as_str()),
            Some("1.2.0")
        );
        Ok(())
    }

    #[test]
    fn yanked_versions_are_skipped() -> Result<(), semver::Error> {
        let lines = vec![line("1.0.0", false), line("1.2.0", true)];
        let req = VersionReq::parse("^1")?;
        assert_eq!(
            pick_line(&lines, &req, None).map(|l| l.vers.as_str()),
            Some("1.0.0")
        );
        let only_yanked = vec![line("1.2.0", true)];
        assert!(pick_line(&only_yanked, &req, None).is_none());
        Ok(())
    }

    #[test]
    fn pinned_version_wins_while_it_matches() -> Result<(), semver::Error> {
        let lines = vec![line("1.0.0", false), line("1.2.0", false)];
        let req = VersionReq::parse("^1")?;
        assert_eq!(
            pick_line(&lines, &req, Some("1.0.0")).map(|l| l.vers.as_str()),
            Some("1.0.0")
        );
        let wide = VersionReq::parse("^2")?;
        assert!(pick_line(&lines, &wide, Some("1.0.0")).is_none());
        Ok(())
    }

    #[test]
    fn requirement_forms_caret_tilde_exact_wildcard() -> Result<(), semver::Error> {
        let lines = vec![
            line("1.2.5", false),
            line("1.3.0", false),
            line("2.0.0", false),
        ];
        let pick = |req: &str| {
            pick_line(&lines, &VersionReq::parse(req).unwrap(), None).map(|l| l.vers.clone())
        };
        assert_eq!(pick("~1.2").as_deref(), Some("1.2.5"));
        assert_eq!(pick("=1.3.0").as_deref(), Some("1.3.0"));
        assert_eq!(pick("1.*").as_deref(), Some("1.3.0"));
        assert_eq!(pick("*").as_deref(), Some("2.0.0"));
        Ok(())
    }

    #[test]
    fn bad_index_line_names_its_location() {
        let err = parse_lines(b"{\"name\":\"json\"}\n", "js/on/json").unwrap_err();
        assert!(err.contains("js/on/json:1"), "{err}");
    }

    #[test]
    fn download_template_fills_name_and_version() {
        assert_eq!(
            download_url("https://x/{name}/{name}-{version}.tar.gz", "json", "1.2.0"),
            "https://x/json/json-1.2.0.tar.gz"
        );
    }

    #[test]
    fn unpack_rejects_non_gzip_bytes() -> io::Result<()> {
        let base = temp_dir("unpack_bad")?;
        let err = unpack(b"not a tarball", &base.join("out"));
        fs::remove_dir_all(&base)?;
        assert!(err.is_err());
        Ok(())
    }
}

#[cfg(test)]
mod end_to_end {
    use super::*;
    use crate::{
        deps,
        git::Store,
        lock, manifest,
        manifest::{FILE_NAME, Manifest},
        package,
        testutil::temp_dir,
        ui::Ui,
    };
    use std::io;

    struct Fixture {
        base: PathBuf,
        dl: PathBuf,
        src: PathBuf,
        home: PathBuf,
        root: PathBuf,
        index: String,
    }

    fn fixture(tag: &str) -> io::Result<Fixture> {
        let base = temp_dir(tag)?;
        let fx = Fixture {
            index: format!("file://{}", base.join("index").display()),
            base: base.join("index"),
            dl: base.join("dl"),
            src: base.join("src"),
            home: base.join("home"),
            root: base.join("app"),
        };
        fs::create_dir_all(&fx.base)?;
        fs::create_dir_all(&fx.dl)?;
        fs::create_dir_all(&fx.root)?;
        fs::write(
            fx.base.join("config.json"),
            format!(
                "{{\"dl\":\"file://{}/{{name}}-{{version}}.tar.gz\"}}",
                fx.dl.display()
            ),
        )?;
        Ok(fx)
    }

    fn manifest_text(name: &str, version: &str, deps: &[(&str, &str)]) -> String {
        let mut text =
            format!("[project]\nname = \"{name}\"\nversion = \"{version}\"\n\n[dependencies]\n");
        for (dep, req) in deps {
            text += &format!("{dep} = \"{req}\"\n");
        }
        text
    }

    /// Packages a gene, writes its tarball, and appends its line to the index.
    fn publish(
        fx: &Fixture,
        name: &str,
        version: &str,
        deps: &[(&str, &str)],
        yanked: bool,
    ) -> io::Result<()> {
        let dir = fx.src.join(format!("{name}-{version}"));
        fs::create_dir_all(dir.join("src"))?;
        fs::write(dir.join(FILE_NAME), manifest_text(name, version, deps))?;
        fs::write(
            dir.join("src/lib.nk"),
            format!("fn {name}() -> i32 {{ return 0; }}\n"),
        )?;
        let manifest = manifest::load(&dir.join(FILE_NAME)).map_err(io::Error::other)?;
        let out = fx.dl.join(format!("{name}-{version}.tar.gz"));
        let built = package::build_archive(&dir, &manifest, &out).map_err(io::Error::other)?;
        let mut line = built.line;
        line.yanked = yanked;
        let file = fx.base.join(index_path(name).map_err(io::Error::other)?);
        fs::create_dir_all(file.parent().unwrap_or(&fx.base))?;
        let mut text = fs::read_to_string(&file).unwrap_or_default();
        text += &serde_json::to_string(&line).map_err(io::Error::other)?;
        text += "\n";
        fs::write(&file, text)
    }

    fn store(fx: &Fixture, offline: bool) -> Store {
        Store::new(fx.home.clone(), offline).with_index(&fx.index)
    }

    fn resolve(fx: &Fixture, deps: &[(&str, &str)], store: &Store) -> Result<deps::Graph, Error> {
        fs::write(fx.root.join(FILE_NAME), manifest_text("app", "0.1.0", deps))
            .map_err(|e| Error::from(e.to_string()))?;
        let manifest = manifest::load(&fx.root.join(FILE_NAME))?;
        deps::collect(&fx.root, &manifest, store)
    }

    fn version_of(graph: &deps::Graph, name: &str) -> Option<String> {
        graph
            .genes
            .iter()
            .find(|gene| gene.name == name)
            .map(|gene| gene.version.clone())
    }

    #[test]
    fn highest_matching_version_is_installed() -> io::Result<()> {
        let fx = fixture("reg_highest")?;
        for v in ["1.0.0", "1.2.0", "2.0.0"] {
            publish(&fx, "json", v, &[], false)?;
        }
        let graph =
            resolve(&fx, &[("json", "^1")], &store(&fx, false)).map_err(io::Error::other)?;
        let installed = fx.home.join("registry/src/json-1.2.0/src/lib.nk").is_file();
        let version = version_of(&graph, "json");
        fs::remove_dir_all(&fx.base.parent().unwrap_or(&fx.base).to_path_buf())?;
        assert_eq!(version.as_deref(), Some("1.2.0"));
        assert!(installed);
        Ok(())
    }

    #[test]
    fn yanked_versions_are_skipped() -> io::Result<()> {
        let fx = fixture("reg_yank")?;
        publish(&fx, "json", "1.2.0", &[], false)?;
        publish(&fx, "json", "1.3.0", &[], true)?;
        let graph =
            resolve(&fx, &[("json", "^1")], &store(&fx, false)).map_err(io::Error::other)?;
        let version = version_of(&graph, "json");
        fs::remove_dir_all(fx.base.parent().unwrap_or(&fx.base))?;
        assert_eq!(version.as_deref(), Some("1.2.0"));
        Ok(())
    }

    #[test]
    fn checksum_mismatch_is_rejected_and_nothing_is_unpacked() -> io::Result<()> {
        let fx = fixture("reg_checksum")?;
        publish(&fx, "json", "1.0.0", &[], false)?;
        fs::write(fx.dl.join("json-1.0.0.tar.gz"), b"tampered")?;
        let err = resolve(&fx, &[("json", "^1")], &store(&fx, false)).err();
        let unpacked = fx.home.join("registry/src/json-1.0.0").exists();
        fs::remove_dir_all(fx.base.parent().unwrap_or(&fx.base))?;
        assert!(err.is_some_and(|e| e.contains("checksum mismatch")));
        assert!(!unpacked);
        Ok(())
    }

    #[test]
    fn transitive_registry_dependencies_are_resolved() -> io::Result<()> {
        let fx = fixture("reg_transitive")?;
        publish(&fx, "json", "1.0.0", &[], false)?;
        publish(&fx, "yaml", "0.4.0", &[("json", "^1")], false)?;
        let graph =
            resolve(&fx, &[("yaml", "^0.4")], &store(&fx, false)).map_err(io::Error::other)?;
        let names: Vec<&str> = graph.genes.iter().map(|g| g.name.as_str()).collect();
        fs::remove_dir_all(fx.base.parent().unwrap_or(&fx.base))?;
        assert_eq!(names, ["json", "yaml"]);
        Ok(())
    }

    #[test]
    fn conflicting_requirements_name_both_sides() -> io::Result<()> {
        let fx = fixture("reg_conflict")?;
        publish(&fx, "json", "1.0.0", &[], false)?;
        publish(&fx, "json", "2.0.0", &[], false)?;
        publish(&fx, "x", "1.0.0", &[("json", "^2")], false)?;
        let err = resolve(&fx, &[("json", "^1"), ("x", "^1")], &store(&fx, false)).err();
        fs::remove_dir_all(fx.base.parent().unwrap_or(&fx.base))?;
        let err = err.unwrap_or_else(|| Error::from("no error"));
        assert!(
            err.contains("'json' is required as ^2 but 1.0.0 was already chosen"),
            "{err}"
        );
        Ok(())
    }

    #[test]
    fn unknown_gene_is_a_clear_error() -> io::Result<()> {
        let fx = fixture("reg_missing")?;
        let err = resolve(&fx, &[("nope", "^1")], &store(&fx, false)).err();
        fs::remove_dir_all(fx.base.parent().unwrap_or(&fx.base))?;
        assert!(err.is_some_and(|e| e.contains("no gene named 'nope'")));
        Ok(())
    }

    #[test]
    fn lock_keeps_the_locked_version_and_round_trips() -> io::Result<()> {
        let fx = fixture("reg_lock")?;
        publish(&fx, "json", "1.0.0", &[], false)?;
        let first =
            resolve(&fx, &[("json", "^1")], &store(&fx, false)).map_err(io::Error::other)?;
        lock::sync(&fx.root, &first, false, Ui::default()).map_err(io::Error::other)?;
        let text = fs::read_to_string(fx.root.join(lock::FILE_NAME))?;

        publish(&fx, "json", "1.2.0", &[], false)?;
        let pins = lock::pins(&fx.root).map_err(io::Error::other)?;
        let pinned = Store::new(fx.home.clone(), false)
            .with_index(&fx.index)
            .with_pins(pins);
        let again = resolve(&fx, &[("json", "^1")], &pinned).map_err(io::Error::other)?;
        let version = version_of(&again, "json");
        fs::remove_dir_all(fx.base.parent().unwrap_or(&fx.base))?;
        assert!(
            text.contains(&format!("source = \"registry+{}\"", fx.index)),
            "{text}"
        );
        assert!(text.contains("checksum = "), "{text}");
        assert!(text.contains("version = \"1.0.0\""), "{text}");
        assert_eq!(version.as_deref(), Some("1.0.0"));
        Ok(())
    }

    #[test]
    fn offline_uses_the_cache_and_fails_without_it() -> io::Result<()> {
        let fx = fixture("reg_offline")?;
        publish(&fx, "json", "1.0.0", &[], false)?;
        let cold = resolve(&fx, &[("json", "^1")], &store(&fx, true)).err();
        resolve(&fx, &[("json", "^1")], &store(&fx, false)).map_err(io::Error::other)?;
        let warm = resolve(&fx, &[("json", "^1")], &store(&fx, true)).map_err(io::Error::other);
        fs::remove_dir_all(fx.base.parent().unwrap_or(&fx.base))?;
        assert!(cold.is_some_and(|e| e.contains("--offline")));
        assert_eq!(version_of(&warm?, "json").as_deref(), Some("1.0.0"));
        Ok(())
    }

    #[test]
    fn search_reads_the_names_list() -> io::Result<()> {
        let fx = fixture("reg_search")?;
        fs::write(fx.base.join("names.txt"), "json\nnanojson\nyaml\n")?;
        let found = store(&fx, false)
            .registry
            .search("JSON")
            .map_err(io::Error::other)?;
        fs::remove_file(fx.base.join("names.txt"))?;
        let missing = store(&fx, false).registry.search("json").err();
        fs::remove_dir_all(fx.base.parent().unwrap_or(&fx.base))?;
        assert_eq!(found, ["json", "nanojson"]);
        assert!(missing.is_some_and(|e| e.contains("no names.txt")));
        Ok(())
    }

    #[test]
    fn index_check_accepts_a_good_index_and_flags_bad_lines() -> io::Result<()> {
        let fx = fixture("reg_check")?;
        publish(&fx, "json", "1.0.0", &[("yaml", "^0.4")], false)?;
        publish(&fx, "yaml", "0.4.0", &[], false)?;
        let good = check_index(&fx.base, true).map_err(io::Error::other);
        let file = fx.base.join(index_path("json").map_err(io::Error::other)?);
        let mut text = fs::read_to_string(&file)?;
        text += "{\"name\":\"json\",\"vers\":\"1.0.0\",\"cksum\":\"zz\"}\n";
        fs::write(&file, text)?;
        let bad = check_index(&fx.base, false).err();
        fs::remove_dir_all(fx.base.parent().unwrap_or(&fx.base))?;
        assert!(good.is_ok_and(|n| n == 2));
        assert!(bad.is_some_and(|e| e.contains("expected 64 hex digits")));
        Ok(())
    }

    #[test]
    fn index_check_flags_missing_dependency_and_wrong_place() -> io::Result<()> {
        let fx = fixture("reg_check2")?;
        publish(&fx, "json", "1.0.0", &[("ghost", "^1")], false)?;
        fs::create_dir_all(fx.base.join("zz"))?;
        fs::write(fx.base.join("zz/json"), "")?;
        let err = check_index(&fx.base, false).err();
        fs::remove_dir_all(fx.base.parent().unwrap_or(&fx.base))?;
        let err = err.unwrap_or_else(|| Error::from("no error"));
        assert!(
            err.contains("dependency 'ghost' is not in the index"),
            "{err}"
        );
        assert!(err.contains("file must live at"), "{err}");
        Ok(())
    }

    #[test]
    fn gene_shorthand_rejects_a_bad_owner_name() -> io::Result<()> {
        let fx = fixture("reg_gene")?;
        fs::write(
            fx.root.join(FILE_NAME),
            "[project]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\njson = { gene = \"nope\" }\n",
        )?;
        let manifest: Manifest =
            manifest::load(&fx.root.join(FILE_NAME)).map_err(io::Error::other)?;
        let err = deps::collect(&fx.root, &manifest, &store(&fx, false)).err();
        fs::remove_dir_all(fx.base.parent().unwrap_or(&fx.base))?;
        assert!(err.is_some_and(|e| e.contains("is not owner/name")));
        Ok(())
    }
}
