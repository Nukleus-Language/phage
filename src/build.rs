use crate::{
    deps::{self, Gene, Graph, Source},
    git::{Store, redact},
    lock,
    manifest::{self, FILE_NAME, LintLevel, Manifest, find_manifest},
    ui::{self, Ui},
};
use std::{
    collections::BTreeMap,
    env,
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

const JIT_REFUSED_EXIT_CODE: i32 = 125;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub release: bool,
    pub locked: bool,
    pub offline: bool,
}

struct Target {
    nukleus: OsString,
    lamina: OsString,
    root: PathBuf,
    manifest: Manifest,
    store: Store,
}

fn resolve(offline: bool, ui: Ui) -> Result<Target, String> {
    let cwd =
        env::current_dir().map_err(|e| format!("Could not read the current directory: {}", e))?;
    let manifest_path = find_manifest(&cwd).ok_or_else(|| {
        ui::with_help(
            format!(
                "No {} found in '{}' or any parent directory.",
                FILE_NAME,
                cwd.display()
            ),
            "run 'phage init' or 'phage new <name>' to create a project",
        )
    })?;
    let root = manifest_path
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let manifest = manifest::load(&manifest_path)?;
    Ok(Target {
        nukleus: env::var_os("NUKLEUS").unwrap_or_else(|| OsString::from("nukleus")),
        lamina: env::var_os("LAMINA").unwrap_or_else(|| OsString::from("lamina")),
        root,
        manifest,
        store: Store::from_env(offline)?.with_ui(ui),
    })
}

fn profile_name(release: bool) -> &'static str {
    if release { "release" } else { "dev" }
}

fn finished(ui: Ui, profile: &str, start: Instant) {
    ui.status(
        "Finished",
        &format!("`{}` profile in {}", profile, ui::elapsed(start)),
    );
}

fn announce(ui: Ui, verb: &str, manifest: &Manifest, root: &Path, graph: &Graph) {
    ui.status(
        verb,
        &package_line(
            &manifest.project.name,
            &manifest.project.version,
            &root.display().to_string(),
        ),
    );
    for gene in &graph.genes {
        ui.status(
            verb,
            &package_line(&gene.name, &gene.version, &gene_location(gene)),
        );
    }
}

fn package_line(name: &str, version: &str, location: &str) -> String {
    format!("{} v{} ({})", name, version, location)
}

fn gene_location(gene: &Gene) -> String {
    match &gene.source {
        Source::Path => gene.dir.display().to_string(),
        Source::Git { url, sha } => format!("{}#{}", redact(url), &sha[..sha.len().min(8)]),
    }
}

const BIN_ROOT: &str = "src/main.nk";

struct Roots {
    bin: Option<PathBuf>,
    lib: Option<PathBuf>,
}

fn roots(root: &Path, manifest: &Manifest) -> Result<Roots, String> {
    let bin = root.join(BIN_ROOT);
    let lib = root.join(manifest.lib_path());
    let roots = Roots {
        bin: bin.is_file().then_some(bin),
        lib: lib.is_file().then_some(lib),
    };
    if roots.bin.is_none() && roots.lib.is_none() {
        return Err(format!(
            "No binary root ({}) or library root ({}) found.",
            BIN_ROOT,
            manifest.lib_path().display()
        ));
    }
    Ok(roots)
}

pub fn test(opts: Options, filter: Option<&str>, ui: Ui) -> Result<i32, String> {
    let start = Instant::now();
    let target = resolve(opts.offline, ui)?;
    sync_lock(&target, opts.locked, ui)?;
    let code = test_with(
        &target.nukleus,
        &target.root,
        &target.manifest,
        &target.store,
        filter,
        ui,
    )?;
    finished(ui, "test", start);
    Ok(code)
}

/// Builds and runs a test runner for each root. Every root runs even if an earlier one fails;
/// the result is the first nonzero exit code, or 0.
fn test_with(
    nukleus: &OsStr,
    root: &Path,
    manifest: &Manifest,
    store: &Store,
    filter: Option<&str>,
    ui: Ui,
) -> Result<i32, String> {
    let roots = roots(root, manifest)?;
    let out_dir = root.join("target").join("test");
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("Could not create '{}': {}", out_dir.display(), e))?;
    let graph = deps::collect(root, manifest, store)?;
    let mut targets: Vec<(&Path, String)> = Vec::new();
    if let Some(bin) = &roots.bin {
        targets.push((bin.as_path(), manifest.project.name.clone()));
    }
    if let Some(lib) = &roots.lib {
        targets.push((lib.as_path(), format!("{}-lib", manifest.project.name)));
    }
    announce(ui, "Compiling", manifest, root, &graph);
    let mut code = 0;
    for (input, name) in targets {
        let path = out_dir.join(name);
        compile(
            nukleus,
            input,
            &Output::Test {
                path: &path,
                filter,
            },
            &graph,
            &manifest.lints,
            ui,
        )?;
        ui.status("Running", &format!("`{}`", path.display()));
        let status = Command::new(&path)
            .current_dir(root)
            .status()
            .map_err(|e| format!("Could not run '{}': {}", path.display(), e))?;
        if code == 0 {
            code = status.code().unwrap_or(1);
        }
    }
    Ok(code)
}

fn sync_lock(target: &Target, locked: bool, ui: Ui) -> Result<(), String> {
    let graph = deps::collect(&target.root, &target.manifest, &target.store)?;
    lock::sync(&target.root, &graph, locked, ui)
}

pub fn build(opts: Options, ui: Ui) -> Result<(), String> {
    let start = Instant::now();
    let target = resolve(opts.offline, ui)?;
    sync_lock(&target, opts.locked, ui)?;
    if let Some(note) = build_roots(
        &target.nukleus,
        &target.root,
        &target.manifest,
        &target.store,
        opts.release,
        ui,
    )? {
        println!("{}", note);
    }
    finished(ui, profile_name(opts.release), start);
    Ok(())
}

fn build_roots(
    nukleus: &OsStr,
    root: &Path,
    manifest: &Manifest,
    store: &Store,
    release: bool,
    ui: Ui,
) -> Result<Option<String>, String> {
    let roots = roots(root, manifest)?;
    if roots.bin.is_some() {
        build_with(nukleus, root, manifest, store, release, ui)?;
    }
    let Some(lib) = roots.lib else {
        return Ok(None);
    };
    check_inputs(nukleus, root, manifest, store, &[lib.as_path()], ui)?;
    Ok(roots.bin.is_none().then(|| {
        format!(
            "No library artifact format yet; checked {}.",
            manifest.lib_path().display()
        )
    }))
}

pub fn run(opts: Options, program_args: &[String], ui: Ui) -> Result<i32, String> {
    let target = resolve(opts.offline, ui)?;
    sync_lock(&target, opts.locked, ui)?;
    run_with(
        &target.nukleus,
        &target.root,
        &target.manifest,
        &target.store,
        opts.release,
        program_args,
        ui,
    )
}

fn run_with(
    nukleus: &OsStr,
    root: &Path,
    manifest: &Manifest,
    store: &Store,
    release: bool,
    program_args: &[String],
    ui: Ui,
) -> Result<i32, String> {
    let start = Instant::now();
    let binary = build_with(nukleus, root, manifest, store, release, ui)?;
    finished(ui, profile_name(release), start);
    ui.status("Running", &format!("`{}`", binary.display()));
    let status = Command::new(&binary)
        .args(program_args)
        .status()
        .map_err(|e| format!("Could not run '{}': {}", binary.display(), e))?;
    Ok(status.code().unwrap_or(1))
}

pub fn run_jit(opts: Options, program_args: &[String], ui: Ui) -> Result<i32, String> {
    if let Some(err) = jit_host_error(env::consts::ARCH, env::consts::OS) {
        return Err(err);
    }
    let target = resolve(opts.offline, ui)?;
    sync_lock(&target, opts.locked, ui)?;
    run_jit_with(
        &target.nukleus,
        &target.lamina,
        &target.root,
        &target.manifest,
        &target.store,
        opts.release,
        program_args,
        ui,
    )
}

fn jit_host_error(arch: &str, os: &str) -> Option<String> {
    (arch != "x86_64" || os != "linux").then(|| {
        ui::with_help(
            format!("'run --jit' runs only on x86_64 Linux for now; this host is {arch} {os}."),
            "use 'phage run' instead",
        )
    })
}

fn run_jit_with(
    nukleus: &OsStr,
    lamina: &OsStr,
    root: &Path,
    manifest: &Manifest,
    store: &Store,
    release: bool,
    program_args: &[String],
    ui: Ui,
) -> Result<i32, String> {
    let start = Instant::now();
    let bin = roots(root, manifest)?
        .bin
        .ok_or_else(|| format!("no binary root: {} not found", BIN_ROOT))?;
    let out_dir = root.join("target").join("jit");
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("Could not create '{}': {}", out_dir.display(), e))?;
    let graph = deps::collect(root, manifest, store)?;
    announce(ui, "Compiling", manifest, root, &graph);
    compile(
        nukleus,
        &bin,
        &Output::Jit { out_dir: &out_dir },
        &graph,
        &manifest.lints,
        ui,
    )?;
    finished(ui, profile_name(release), start);
    let ir = out_dir
        .join(bin.file_stem().unwrap_or_default())
        .with_extension("lamina");
    let (program, args) = jit_command(lamina, &ir, release, program_args);
    ui.status(
        "Running",
        &format!("`{}`", ui::render_command(&program, &args)),
    );
    let status = Command::new(&program)
        .args(&args)
        .env("LAMINA_JIT_NO_FALLBACK", "1")
        .status()
        .map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                ui::with_help(
                    format!("lamina not found ('{}')", program.to_string_lossy()),
                    "put it on PATH or set LAMINA to its path",
                )
            } else {
                format!("Could not run '{}': {}", program.to_string_lossy(), e)
            }
        })?;
    match status.code().unwrap_or(1) {
        JIT_REFUSED_EXIT_CODE => Err(ui::with_help(
            "lamina could not JIT this program (its reason is above)",
            "use 'phage run' to build it ahead of time instead",
        )),
        code => Ok(code),
    }
}

fn jit_command(
    lamina: &OsStr,
    ir: &Path,
    release: bool,
    program_args: &[String],
) -> (OsString, Vec<OsString>) {
    let mut args = vec![ir.as_os_str().to_os_string(), OsString::from("--jit")];
    if release {
        args.push(OsString::from("--opt-level"));
        args.push(OsString::from("2"));
    }
    if !program_args.is_empty() {
        args.push(OsString::from("--"));
        args.extend(program_args.iter().map(OsString::from));
    }
    (lamina.to_os_string(), args)
}

pub fn check(opts: Options, ui: Ui) -> Result<(), String> {
    let start = Instant::now();
    let target = resolve(opts.offline, ui)?;
    sync_lock(&target, opts.locked, ui)?;
    check_with(
        &target.nukleus,
        &target.root,
        &target.manifest,
        &target.store,
        ui,
    )?;
    finished(ui, profile_name(false), start);
    Ok(())
}

fn check_with(
    nukleus: &OsStr,
    root: &Path,
    manifest: &Manifest,
    store: &Store,
    ui: Ui,
) -> Result<(), String> {
    let roots = roots(root, manifest)?;
    let inputs: Vec<&Path> = roots
        .bin
        .iter()
        .chain(roots.lib.iter())
        .map(PathBuf::as_path)
        .collect();
    check_inputs(nukleus, root, manifest, store, &inputs, ui)
}

fn check_inputs(
    nukleus: &OsStr,
    root: &Path,
    manifest: &Manifest,
    store: &Store,
    inputs: &[&Path],
    ui: Ui,
) -> Result<(), String> {
    let out_dir = root.join("target").join("check");
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("Could not create '{}': {}", out_dir.display(), e))?;
    let graph = deps::collect(root, manifest, store)?;
    if !inputs.is_empty() {
        announce(ui, "Checking", manifest, root, &graph);
    }
    for input in inputs {
        compile(
            nukleus,
            input,
            &Output::Check { out_dir: &out_dir },
            &graph,
            &manifest.lints,
            ui,
        )?;
    }
    Ok(())
}

fn build_with(
    nukleus: &OsStr,
    root: &Path,
    manifest: &Manifest,
    store: &Store,
    release: bool,
    ui: Ui,
) -> Result<PathBuf, String> {
    let bin = roots(root, manifest)?
        .bin
        .ok_or_else(|| format!("no binary root: {} not found", BIN_ROOT))?;
    let profile = if release { "release" } else { "debug" };
    let out_dir = root.join("target").join(profile);
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("Could not create '{}': {}", out_dir.display(), e))?;
    let out = out_dir.join(&manifest.project.name);
    let graph = deps::collect(root, manifest, store)?;
    announce(ui, "Compiling", manifest, root, &graph);
    compile(
        nukleus,
        &bin,
        &Output::Binary {
            path: &out,
            release,
        },
        &graph,
        &manifest.lints,
        ui,
    )?;
    Ok(out)
}

fn compile(
    nukleus: &OsStr,
    input: &Path,
    output: &Output,
    graph: &Graph,
    lints: &BTreeMap<String, LintLevel>,
    ui: Ui,
) -> Result<(), String> {
    let (program, args) = compile_command(&CompileCommand {
        nukleus,
        input,
        output,
        graph,
        lints,
    });
    ui.verbose(
        "Running",
        &format!("`{}`", ui::render_command(&program, &args)),
    );
    let status = Command::new(&program).args(&args).status().map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            ui::with_help(
                format!("nukleus not found ('{}')", program.to_string_lossy()),
                "put it on PATH or set NUKLEUS to its path",
            )
        } else {
            format!("Could not run '{}': {}", program.to_string_lossy(), e)
        }
    })?;
    if !status.success() {
        return Err(output.failure().to_string());
    }
    Ok(())
}

enum Output<'a> {
    Binary {
        path: &'a Path,
        release: bool,
    },
    Check {
        out_dir: &'a Path,
    },
    Jit {
        out_dir: &'a Path,
    },
    Test {
        path: &'a Path,
        filter: Option<&'a str>,
    },
}

impl Output<'_> {
    fn failure(&self) -> &'static str {
        match self {
            Output::Binary { .. } | Output::Jit { .. } => "build failed",
            Output::Check { .. } => "check failed",
            Output::Test { .. } => "test build failed",
        }
    }
}

struct CompileCommand<'a> {
    nukleus: &'a OsStr,
    input: &'a Path,
    output: &'a Output<'a>,
    graph: &'a Graph,
    lints: &'a BTreeMap<String, LintLevel>,
}

fn compile_command(cmd: &CompileCommand) -> (OsString, Vec<OsString>) {
    let mut args = Vec::new();
    match cmd.output {
        Output::Binary { path, release } => {
            args.push(OsString::from("-o"));
            args.push(path.into());
            if *release {
                args.push(OsString::from("-O2"));
            }
        }
        Output::Check { out_dir } | Output::Jit { out_dir } => {
            args.push(OsString::from("--emit"));
            args.push(OsString::from("ir"));
            args.push(OsString::from("--out-dir"));
            args.push(out_dir.into());
        }
        Output::Test { path, filter } => {
            args.push(OsString::from("--test"));
            if let Some(filter) = filter {
                args.push(OsString::from("--test-filter"));
                args.push(OsString::from(filter));
            }
            args.push(OsString::from("-o"));
            args.push(path.into());
        }
    }
    for (name, path) in &cmd.graph.deps {
        args.push(OsString::from("--dep"));
        args.push(pair(name, path.as_os_str()));
    }
    for (name, level) in cmd.lints {
        args.push(OsString::from("--lint"));
        args.push(pair(name, OsStr::new(level.as_str())));
    }
    for gene in &cmd.graph.genes {
        gene_args(&mut args, gene);
    }
    args.push(cmd.input.into());
    (cmd.nukleus.to_os_string(), args)
}

fn gene_args(args: &mut Vec<OsString>, gene: &Gene) {
    for (name, path) in &gene.deps {
        args.push(OsString::from("--gene-dep"));
        args.push(pair(&format!("{}:{}", gene.name, name), path.as_os_str()));
    }
    for (name, level) in &gene.lints {
        args.push(OsString::from("--gene-lint"));
        args.push(pair(
            &format!("{}:{}", gene.name, name),
            OsStr::new(level.as_str()),
        ));
    }
}

fn pair(name: &str, value: &OsStr) -> OsString {
    let mut text = OsString::from(name);
    text.push("=");
    text.push(value);
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command<'a>(
        out: &'a Path,
        release: bool,
        graph: &'a Graph,
        lints: &'a BTreeMap<String, LintLevel>,
    ) -> (OsString, Vec<OsString>) {
        compile_command(&CompileCommand {
            nukleus: OsStr::new("nukleus"),
            input: Path::new("/p/src/main.nk"),
            output: &Output::Binary { path: out, release },
            graph,
            lints,
        })
    }

    #[test]
    fn compile_command_check_emits_ir_only() {
        let (program, args) = compile_command(&CompileCommand {
            nukleus: OsStr::new("nukleus"),
            input: Path::new("/p/src/main.nk"),
            output: &Output::Check {
                out_dir: Path::new("/p/target/check"),
            },
            graph: &no_graph(),
            lints: &BTreeMap::new(),
        });
        assert_eq!(program, OsString::from("nukleus"));
        assert_eq!(
            args,
            vec![
                OsString::from("--emit"),
                OsString::from("ir"),
                OsString::from("--out-dir"),
                OsString::from("/p/target/check"),
                OsString::from("/p/src/main.nk"),
            ]
        );
    }

    #[test]
    fn compile_command_jit_emits_ir_only() {
        let (program, args) = compile_command(&CompileCommand {
            nukleus: OsStr::new("nukleus"),
            input: Path::new("/p/src/main.nk"),
            output: &Output::Jit {
                out_dir: Path::new("/p/target/jit"),
            },
            graph: &no_graph(),
            lints: &BTreeMap::new(),
        });
        assert_eq!(program, OsString::from("nukleus"));
        assert_eq!(
            args,
            vec![
                OsString::from("--emit"),
                OsString::from("ir"),
                OsString::from("--out-dir"),
                OsString::from("/p/target/jit"),
                OsString::from("/p/src/main.nk"),
            ]
        );
    }

    #[test]
    fn jit_command_debug_leaves_lamina_at_its_default_level() {
        let (program, args) = jit_command(
            OsStr::new("lamina"),
            Path::new("/p/target/jit/main.lamina"),
            false,
            &[],
        );
        assert_eq!(program, OsString::from("lamina"));
        assert_eq!(
            args,
            vec![
                OsString::from("/p/target/jit/main.lamina"),
                OsString::from("--jit"),
            ]
        );
    }

    #[test]
    fn jit_command_release_asks_lamina_for_level_two() {
        let (_, args) = jit_command(
            OsStr::new("lamina"),
            Path::new("/p/target/jit/main.lamina"),
            true,
            &[],
        );
        assert_eq!(
            args,
            vec![
                OsString::from("/p/target/jit/main.lamina"),
                OsString::from("--jit"),
                OsString::from("--opt-level"),
                OsString::from("2"),
            ]
        );
    }

    #[test]
    fn jit_command_passes_program_args_after_dashes() {
        let (_, args) = jit_command(
            OsStr::new("lamina"),
            Path::new("/p/target/jit/main.lamina"),
            false,
            &["a".to_string(), "b c".to_string()],
        );
        assert_eq!(
            args,
            vec![
                OsString::from("/p/target/jit/main.lamina"),
                OsString::from("--jit"),
                OsString::from("--"),
                OsString::from("a"),
                OsString::from("b c"),
            ]
        );
    }

    #[test]
    fn jit_runs_only_on_x86_64_linux() {
        assert_eq!(jit_host_error("x86_64", "linux"), None);
        let err = jit_host_error("aarch64", "linux").unwrap();
        assert!(
            err.contains("x86_64 Linux") && err.contains("aarch64 linux"),
            "{err}"
        );
        assert!(jit_host_error("x86_64", "macos").is_some());
    }

    fn no_graph() -> Graph {
        Graph {
            deps: Vec::new(),
            genes: Vec::new(),
        }
    }

    #[test]
    fn compile_command_test_builds_runner_with_filter() {
        let (program, args) = compile_command(&CompileCommand {
            nukleus: OsStr::new("nukleus"),
            input: Path::new("/p/src/main.nk"),
            output: &Output::Test {
                path: Path::new("/p/target/test/demo"),
                filter: Some("geom"),
            },
            graph: &no_graph(),
            lints: &BTreeMap::new(),
        });
        assert_eq!(program, OsString::from("nukleus"));
        assert_eq!(
            args,
            vec![
                OsString::from("--test"),
                OsString::from("--test-filter"),
                OsString::from("geom"),
                OsString::from("-o"),
                OsString::from("/p/target/test/demo"),
                OsString::from("/p/src/main.nk"),
            ]
        );
    }

    #[test]
    fn compile_command_debug() {
        let (program, args) = command(
            Path::new("/p/target/debug/demo"),
            false,
            &no_graph(),
            &BTreeMap::new(),
        );
        assert_eq!(program, OsString::from("nukleus"));
        assert_eq!(
            args,
            vec![
                OsString::from("-o"),
                OsString::from("/p/target/debug/demo"),
                OsString::from("/p/src/main.nk"),
            ]
        );
    }

    #[test]
    fn compile_command_release_optimizes() {
        let (_, args) = command(
            Path::new("/p/target/release/demo"),
            true,
            &no_graph(),
            &BTreeMap::new(),
        );
        assert_eq!(
            args,
            vec![
                OsString::from("-o"),
                OsString::from("/p/target/release/demo"),
                OsString::from("-O2"),
                OsString::from("/p/src/main.nk"),
            ]
        );
    }

    #[test]
    fn compile_command_passes_dependencies_and_lints() {
        let graph = Graph {
            deps: vec![("json".to_string(), PathBuf::from("/p/json/src/lib.nk"))],
            genes: Vec::new(),
        };
        let lints = BTreeMap::from([
            ("never_mutated".to_string(), LintLevel::Warn),
            ("future_lint".to_string(), LintLevel::Deny),
        ]);
        let (_, args) = command(Path::new("/p/target/debug/demo"), false, &graph, &lints);
        assert_eq!(
            args,
            vec![
                OsString::from("-o"),
                OsString::from("/p/target/debug/demo"),
                OsString::from("--dep"),
                OsString::from("json=/p/json/src/lib.nk"),
                OsString::from("--lint"),
                OsString::from("future_lint=deny"),
                OsString::from("--lint"),
                OsString::from("never_mutated=warn"),
                OsString::from("/p/src/main.nk"),
            ]
        );
    }

    #[test]
    fn compile_command_passes_each_genes_deps_and_lints() {
        let graph = Graph {
            deps: vec![("a".to_string(), PathBuf::from("/p/a/src/lib.nk"))],
            genes: vec![
                Gene {
                    name: "a".to_string(),
                    version: "0.1.0".to_string(),
                    root: PathBuf::from("/p/a/src/lib.nk"),
                    deps: vec![("b".to_string(), PathBuf::from("/p/b/src/lib.nk"))],
                    lints: BTreeMap::from([("never_mutated".to_string(), LintLevel::Deny)]),
                    dir: PathBuf::from("/p/a"),
                    source: deps::Source::Path,
                },
                Gene {
                    name: "b".to_string(),
                    version: "0.1.0".to_string(),
                    root: PathBuf::from("/p/b/src/lib.nk"),
                    deps: Vec::new(),
                    lints: BTreeMap::new(),
                    dir: PathBuf::from("/p/b"),
                    source: deps::Source::Path,
                },
            ],
        };
        let root_lints = BTreeMap::from([("never_mutated".to_string(), LintLevel::Warn)]);
        let (_, args) = command(
            Path::new("/p/target/debug/demo"),
            false,
            &graph,
            &root_lints,
        );
        assert_eq!(
            args,
            vec![
                OsString::from("-o"),
                OsString::from("/p/target/debug/demo"),
                OsString::from("--dep"),
                OsString::from("a=/p/a/src/lib.nk"),
                OsString::from("--lint"),
                OsString::from("never_mutated=warn"),
                OsString::from("--gene-dep"),
                OsString::from("a:b=/p/b/src/lib.nk"),
                OsString::from("--gene-lint"),
                OsString::from("a:never_mutated=deny"),
                OsString::from("/p/src/main.nk"),
            ]
        );
    }

    #[cfg(unix)]
    mod fake {
        use super::*;

        fn store(root: &Path) -> Store {
            Store::new(root.join("home"), false)
        }
        use crate::{manifest::Project, testutil::temp_dir};
        use std::{os::unix::fs::PermissionsExt, sync::Mutex};

        static SPAWN_LOCK: Mutex<()> = Mutex::new(());

        fn lock() -> std::sync::MutexGuard<'static, ()> {
            SPAWN_LOCK.lock().unwrap_or_else(|e| e.into_inner())
        }

        const FAKE_NUKLEUS: &str = "#!/bin/sh
out=\"\"
while [ $# -gt 0 ]; do
  if [ \"$1\" = \"-o\" ]; then out=\"$2\"; shift; fi
  shift
done
printf '#!/bin/sh\\necho built\\nexit 7\\n' > \"$out\"
chmod +x \"$out\"
";

        fn write_exec(path: &Path, text: &str) -> io::Result<()> {
            fs::write(path, text)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        }

        fn project(tag: &str) -> io::Result<PathBuf> {
            let root = temp_dir(tag)?;
            fs::create_dir_all(root.join("src"))?;
            fs::write(root.join("src/main.nk"), "")?;
            Ok(root)
        }

        fn demo() -> Manifest {
            Manifest {
                project: Project {
                    name: "demo".to_string(),
                    version: "0.1.0".to_string(),
                },
                lib: None,
                dependencies: BTreeMap::new(),
                lints: BTreeMap::new(),
            }
        }

        #[test]
        fn build_creates_debug_binary() -> io::Result<()> {
            let _guard = lock();
            let root = project("build")?;
            let nukleus = root.join("fake-nukleus");
            write_exec(&nukleus, FAKE_NUKLEUS)?;

            let out = build_with(
                nukleus.as_os_str(),
                &root,
                &demo(),
                &store(&root),
                false,
                Ui::default(),
            )
            .map_err(io::Error::other)?;
            assert_eq!(out, root.join("target/debug/demo"));
            assert!(out.is_file());
            fs::remove_dir_all(&root)?;
            Ok(())
        }

        #[test]
        fn build_reports_nukleus_failure() -> io::Result<()> {
            let _guard = lock();
            let root = project("fail")?;
            let nukleus = root.join("fake-nukleus");
            write_exec(&nukleus, "#!/bin/sh\nexit 3\n")?;

            let err = build_with(
                nukleus.as_os_str(),
                &root,
                &demo(),
                &store(&root),
                true,
                Ui::default(),
            )
            .unwrap_err();
            assert_eq!(err, "build failed");
            fs::remove_dir_all(&root)?;
            Ok(())
        }

        #[test]
        fn run_returns_program_exit_code() -> io::Result<()> {
            let _guard = lock();
            let root = project("run")?;
            let nukleus = root.join("fake-nukleus");
            write_exec(&nukleus, FAKE_NUKLEUS)?;

            let code = run_with(
                nukleus.as_os_str(),
                &root,
                &demo(),
                &store(&root),
                true,
                &[],
                Ui::default(),
            )
            .map_err(io::Error::other)?;
            assert_eq!(code, 7);
            fs::remove_dir_all(&root)?;
            Ok(())
        }

        #[test]
        fn build_reports_missing_nukleus() -> io::Result<()> {
            let _guard = lock();
            let root = project("missing")?;
            let nukleus = root.join("no-such-nukleus");

            let err = build_with(
                nukleus.as_os_str(),
                &root,
                &demo(),
                &store(&root),
                false,
                Ui::default(),
            )
            .unwrap_err();
            assert!(err.contains("NUKLEUS"), "{err}");
            fs::remove_dir_all(&root)?;
            Ok(())
        }

        const FAKE_CHECK: &str = "#!/bin/sh
dir=\"\"
last=\"\"
while [ $# -gt 0 ]; do
  if [ \"$1\" = \"--out-dir\" ]; then dir=\"$2\"; shift; fi
  last=\"$1\"
  shift
done
mkdir -p \"$dir\" && echo ir > \"$dir/$(basename \"$last\" .nk).lamina\"
";

        fn lib_only(tag: &str) -> io::Result<PathBuf> {
            let root = project(tag)?;
            fs::remove_file(root.join("src/main.nk"))?;
            fs::write(root.join("src/lib.nk"), "")?;
            Ok(root)
        }

        #[test]
        fn check_lib_only_checks_lib_root() -> io::Result<()> {
            let _guard = lock();
            let root = lib_only("check_lib")?;
            let nukleus = root.join("fake-nukleus");
            write_exec(&nukleus, FAKE_CHECK)?;

            check_with(
                nukleus.as_os_str(),
                &root,
                &demo(),
                &store(&root),
                Ui::default(),
            )
            .map_err(io::Error::other)?;
            let written = root.join("target/check/lib.lamina").is_file();
            fs::remove_dir_all(&root)?;
            assert!(written);
            Ok(())
        }

        #[test]
        fn check_checks_both_roots_when_present() -> io::Result<()> {
            let _guard = lock();
            let root = project("check_both")?;
            fs::write(root.join("src/lib.nk"), "")?;
            let nukleus = root.join("fake-nukleus");
            write_exec(&nukleus, FAKE_CHECK)?;

            check_with(
                nukleus.as_os_str(),
                &root,
                &demo(),
                &store(&root),
                Ui::default(),
            )
            .map_err(io::Error::other)?;
            let written = root.join("target/check/main.lamina").is_file()
                && root.join("target/check/lib.lamina").is_file();
            fs::remove_dir_all(&root)?;
            assert!(written);
            Ok(())
        }

        #[test]
        fn build_lib_only_checks_and_notes() -> io::Result<()> {
            let _guard = lock();
            let root = lib_only("build_lib")?;
            let nukleus = root.join("fake-nukleus");
            write_exec(&nukleus, FAKE_CHECK)?;

            let note = build_roots(
                nukleus.as_os_str(),
                &root,
                &demo(),
                &store(&root),
                false,
                Ui::default(),
            )
            .map_err(io::Error::other)?;
            let checked = root.join("target/check/lib.lamina").is_file();
            let no_binary = !root.join("target/debug/demo").exists();
            fs::remove_dir_all(&root)?;
            assert_eq!(
                note.as_deref(),
                Some("No library artifact format yet; checked src/lib.nk.")
            );
            assert!(checked && no_binary);
            Ok(())
        }

        #[test]
        fn build_with_binary_root_has_no_note() -> io::Result<()> {
            let _guard = lock();
            let root = project("build_bin")?;
            let nukleus = root.join("fake-nukleus");
            write_exec(&nukleus, FAKE_NUKLEUS)?;

            let note = build_roots(
                nukleus.as_os_str(),
                &root,
                &demo(),
                &store(&root),
                false,
                Ui::default(),
            )
            .map_err(io::Error::other)?;
            let built = root.join("target/debug/demo").is_file();
            fs::remove_dir_all(&root)?;
            assert_eq!(note, None);
            assert!(built);
            Ok(())
        }

        #[test]
        fn run_lib_only_reports_no_binary_root() -> io::Result<()> {
            let _guard = lock();
            let root = lib_only("run_lib")?;
            let nukleus = root.join("fake-nukleus");
            write_exec(&nukleus, FAKE_CHECK)?;

            let err = run_with(
                nukleus.as_os_str(),
                &root,
                &demo(),
                &store(&root),
                false,
                &[],
                Ui::default(),
            )
            .unwrap_err();
            let checked = root.join("target/check/lib.lamina").exists();
            fs::remove_dir_all(&root)?;
            assert!(err.starts_with("no binary root"), "{err}");
            assert!(!checked);
            Ok(())
        }

        #[test]
        fn roots_error_without_any_root() -> io::Result<()> {
            let root = project("no_roots")?;
            fs::remove_file(root.join("src/main.nk"))?;

            let err = roots(&root, &demo()).err().unwrap_or_default();
            fs::remove_dir_all(&root)?;
            assert!(err.contains("No binary root"), "{err}");
            Ok(())
        }

        #[test]
        fn check_writes_ir_under_target_check() -> io::Result<()> {
            let _guard = lock();
            let root = project("check")?;
            let nukleus = root.join("fake-nukleus");
            write_exec(&nukleus, FAKE_CHECK)?;

            check_with(
                nukleus.as_os_str(),
                &root,
                &demo(),
                &store(&root),
                Ui::default(),
            )
            .map_err(io::Error::other)?;
            assert!(root.join("target/check/main.lamina").is_file());
            fs::remove_dir_all(&root)?;
            Ok(())
        }

        #[test]
        fn test_runs_runner_and_returns_its_exit_code() -> io::Result<()> {
            let _guard = lock();
            let root = project("test_run")?;
            let nukleus = root.join("fake-nukleus");
            write_exec(&nukleus, FAKE_NUKLEUS)?;

            let code = test_with(
                nukleus.as_os_str(),
                &root,
                &demo(),
                &store(&root),
                None,
                Ui::default(),
            )
            .map_err(io::Error::other)?;
            assert_eq!(code, 7);
            assert!(root.join("target/test/demo").is_file());
            Ok(())
        }

        #[test]
        fn test_lib_only_builds_lib_runner() -> io::Result<()> {
            let _guard = lock();
            let root = lib_only("test_lib")?;
            let nukleus = root.join("fake-nukleus");
            write_exec(&nukleus, FAKE_NUKLEUS)?;

            let code = test_with(
                nukleus.as_os_str(),
                &root,
                &demo(),
                &store(&root),
                Some("x"),
                Ui::default(),
            )
            .map_err(io::Error::other)?;
            assert_eq!(code, 7);
            assert!(root.join("target/test/demo-lib").is_file());
            assert!(!root.join("target/test/demo").exists());
            Ok(())
        }

        #[test]
        fn test_reports_runner_build_failure() -> io::Result<()> {
            let _guard = lock();
            let root = project("test_fail")?;
            let nukleus = root.join("fake-nukleus");
            write_exec(&nukleus, "#!/bin/sh\nexit 1\n")?;

            let err = test_with(
                nukleus.as_os_str(),
                &root,
                &demo(),
                &store(&root),
                None,
                Ui::default(),
            )
            .unwrap_err();
            assert_eq!(err, "test build failed");
            Ok(())
        }

        #[test]
        fn check_reports_nukleus_error() -> io::Result<()> {
            let _guard = lock();
            let root = project("check_fail")?;
            let nukleus = root.join("fake-nukleus");
            write_exec(&nukleus, "#!/bin/sh\nexit 1\n")?;

            let err = check_with(
                nukleus.as_os_str(),
                &root,
                &demo(),
                &store(&root),
                Ui::default(),
            )
            .unwrap_err();
            assert_eq!(err, "check failed");
            fs::remove_dir_all(&root)?;
            Ok(())
        }
        const FAKE_LAMINA: &str = "#!/bin/sh
[ \"$2\" = \"--jit\" ] || exit 4
case \"$1\" in *main.lamina) ;; *) exit 5 ;; esac
[ \"$LAMINA_JIT_NO_FALLBACK\" = 1 ] || exit 6
exit 7
";

        #[test]
        fn run_jit_builds_ir_and_returns_lamina_exit_code() -> io::Result<()> {
            let _guard = lock();
            let root = project("jit")?;
            let nukleus = root.join("fake-nukleus");
            let lamina = root.join("fake-lamina");
            write_exec(&nukleus, FAKE_CHECK)?;
            write_exec(&lamina, FAKE_LAMINA)?;

            let code = run_jit_with(
                nukleus.as_os_str(),
                lamina.as_os_str(),
                &root,
                &demo(),
                &store(&root),
                false,
                &[],
                Ui::default(),
            )
            .map_err(io::Error::other)?;
            assert_eq!(code, 7);
            assert!(root.join("target/jit/main.lamina").is_file());
            Ok(())
        }

        #[test]
        fn run_jit_passes_program_args_to_lamina() -> io::Result<()> {
            let _guard = lock();
            let root = project("jit_args")?;
            let nukleus = root.join("fake-nukleus");
            let lamina = root.join("fake-lamina");
            write_exec(&nukleus, FAKE_CHECK)?;
            write_exec(
                &lamina,
                "#!/bin/sh\n[ \"$#\" = 5 ] && [ \"$3\" = \"--\" ] && [ \"$4\" = a ] && [ \"$5\" = \"b c\" ] || exit 8\nexit 7\n",
            )?;

            let code = run_jit_with(
                nukleus.as_os_str(),
                lamina.as_os_str(),
                &root,
                &demo(),
                &store(&root),
                false,
                &["a".to_string(), "b c".to_string()],
                Ui::default(),
            )
            .map_err(io::Error::other)?;
            assert_eq!(code, 7);
            Ok(())
        }

        #[test]
        fn run_jit_reports_refusal_with_suggestion() -> io::Result<()> {
            let _guard = lock();
            let root = project("jit_refused")?;
            let nukleus = root.join("fake-nukleus");
            let lamina = root.join("fake-lamina");
            write_exec(&nukleus, FAKE_CHECK)?;
            write_exec(
                &lamina,
                "#!/bin/sh\necho 'Error: ValidationError(\"no calls\")' >&2\nexit 125\n",
            )?;

            let err = run_jit_with(
                nukleus.as_os_str(),
                lamina.as_os_str(),
                &root,
                &demo(),
                &store(&root),
                false,
                &[],
                Ui::default(),
            )
            .unwrap_err();
            assert!(err.contains("phage run"), "{err}");
            Ok(())
        }

        #[test]
        fn run_jit_program_exit_one_is_not_a_refusal() -> io::Result<()> {
            let _guard = lock();
            let root = project("jit_exit_one")?;
            let nukleus = root.join("fake-nukleus");
            let lamina = root.join("fake-lamina");
            write_exec(&nukleus, FAKE_CHECK)?;
            write_exec(&lamina, "#!/bin/sh\necho done >&2\nexit 1\n")?;

            let code = run_jit_with(
                nukleus.as_os_str(),
                lamina.as_os_str(),
                &root,
                &demo(),
                &store(&root),
                false,
                &[],
                Ui::default(),
            )
            .map_err(io::Error::other)?;
            assert_eq!(code, 1);
            Ok(())
        }

        #[test]
        fn run_jit_reports_missing_lamina() -> io::Result<()> {
            let _guard = lock();
            let root = project("jit_missing")?;
            let nukleus = root.join("fake-nukleus");
            write_exec(&nukleus, FAKE_CHECK)?;
            let lamina = root.join("no-such-lamina");

            let err = run_jit_with(
                nukleus.as_os_str(),
                lamina.as_os_str(),
                &root,
                &demo(),
                &store(&root),
                false,
                &[],
                Ui::default(),
            )
            .unwrap_err();
            assert!(err.contains("LAMINA"), "{err}");
            Ok(())
        }
    }
}
