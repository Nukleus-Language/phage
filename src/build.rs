use crate::{
    deps::{self, Gene, Graph, Source},
    error::Error,
    fingerprint::{self, Stamp},
    git::{Store, redact},
    lock,
    manifest::{FILE_NAME, LintLevel, Manifest},
    profile::{self, Profile},
    ui::{self, Ui},
    workspace::{Package, Workspace},
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
const BIN_ROOT: &str = "src/main.nk";

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Options {
    pub release: bool,
    pub locked: bool,
    pub offline: bool,
    pub profile: Option<String>,
    pub packages: Vec<String>,
}

struct Env {
    nukleus: OsString,
    lamina: OsString,
    store: Store,
    target: PathBuf,
    profile: Profile,
    ui: Ui,
}

impl Env {
    fn new(ws: &Workspace, opts: &Options, ui: Ui) -> Result<Env, Error> {
        Ok(Env {
            nukleus: env::var_os("NUKLEUS").unwrap_or_else(|| OsString::from("nukleus")),
            lamina: env::var_os("LAMINA").unwrap_or_else(|| OsString::from("lamina")),
            store: Store::from_env(opts.offline)?
                .with_ui(ui)
                .with_pins(lock::pins(&ws.root)?),
            target: ws.root.join("target"),
            profile: Profile::resolve(profile_name(opts)?, &ws.manifest.profile)?,
            ui,
        })
    }

    fn profile_key(&self) -> String {
        format!("{}:{}", self.profile.name, self.profile.opt_level)
    }
}

fn profile_name(opts: &Options) -> Result<&str, Error> {
    match (opts.profile.as_deref(), opts.release) {
        (None, false) => Ok("dev"),
        (None, true) => Ok("release"),
        (Some(name), false) => Ok(name),
        (Some("release"), true) => Ok("release"),
        (Some(name), true) => Err(Error::with_help(
            format!("--release conflicts with --profile {}", name),
            "use --profile release, or drop --release",
        )),
    }
}

fn discover() -> Result<Workspace, Error> {
    let cwd = env::current_dir()
        .map_err(|e| Error::from(format!("Could not read the current directory: {}", e)))?;
    Workspace::discover(&cwd)
}

struct Roots {
    bin: Option<PathBuf>,
    lib: Option<PathBuf>,
}

fn roots(root: &Path, manifest: &Manifest) -> Result<Roots, Error> {
    let bin = root.join(BIN_ROOT);
    let lib = root.join(manifest.lib_path());
    let roots = Roots {
        bin: bin.is_file().then_some(bin),
        lib: lib.is_file().then_some(lib),
    };
    if roots.bin.is_none() && roots.lib.is_none() {
        return Err(Error::from(format!(
            "No binary root ({}) or library root ({}) found.",
            BIN_ROOT,
            manifest.lib_path().display()
        )));
    }
    Ok(roots)
}

fn make_dir(dir: &Path) -> Result<(), Error> {
    fs::create_dir_all(dir)
        .map_err(|e| Error::from(format!("Could not create '{}': {}", dir.display(), e)))
}

fn finished(ui: Ui, profile: &str, start: Instant) {
    ui.status(
        "Finished",
        &format!("`{}` profile in {}", profile, ui::elapsed(start)),
    );
}

fn sync_lock(env: &Env, ws: &Workspace, locked: bool) -> Result<(), Error> {
    let mut genes: Vec<Gene> = Vec::new();
    for package in &ws.packages {
        for gene in deps::collect(&package.dir, &package.manifest, &env.store)?.genes {
            let seen = genes
                .iter()
                .find(|seen| seen.name == gene.name)
                .map(|seen| seen.dir.clone());
            match seen {
                Some(dir) if dir != gene.dir => {
                    return Err(Error::from(format!(
                        "Dependency '{}' resolves to both '{}' and '{}'",
                        gene.name,
                        dir.display(),
                        gene.dir.display()
                    )));
                }
                Some(_) => {}
                None => genes.push(gene),
            }
        }
    }
    lock::sync(
        &ws.root,
        &Graph {
            deps: Vec::new(),
            genes,
        },
        locked,
        env.ui,
    )
}

fn single_package<'a>(ws: &'a Workspace, names: &[String]) -> Result<&'a Package, Error> {
    let selected = ws.select(names)?;
    match selected.as_slice() {
        [package] => Ok(*package),
        _ => Err(Error::with_help(
            "run needs exactly one package",
            format!(
                "pass -p <name>; packages: {}",
                ws.package_names().join(", ")
            ),
        )),
    }
}

pub fn test(opts: Options, filter: Option<&str>, ui: Ui) -> Result<i32, Error> {
    let start = Instant::now();
    let ws = discover()?;
    let env = Env::new(&ws, &opts, ui)?;
    sync_lock(&env, &ws, opts.locked)?;
    let mut code = 0;
    for package in ws.select(&opts.packages)? {
        let status = test_with(&env, package, filter)?;
        if code == 0 {
            code = status;
        }
    }
    finished(ui, "test", start);
    Ok(code)
}

/// Builds and runs a test runner for each root. Every root runs even if an earlier one fails;
/// the result is the first nonzero exit code, or 0.
fn test_with(env: &Env, package: &Package, filter: Option<&str>) -> Result<i32, Error> {
    let roots = roots(&package.dir, &package.manifest)?;
    let out_dir = env.target.join("test");
    make_dir(&out_dir)?;
    let graph = deps::collect(&package.dir, &package.manifest, &env.store)?;
    let sources = fingerprint::sources(&env.nukleus, &package.dir, &graph, &env.profile_key())?;
    let mut targets: Vec<(&Path, String)> = Vec::new();
    if let Some(bin) = &roots.bin {
        targets.push((bin.as_path(), package.manifest.name().to_string()));
    }
    if let Some(lib) = &roots.lib {
        targets.push((lib.as_path(), format!("{}-lib", package.manifest.name())));
    }
    let jobs: Vec<(PathBuf, Job)> = targets
        .iter()
        .map(|(input, name)| {
            let path = out_dir.join(name);
            let job = plan(
                env,
                &package.manifest.lints,
                input,
                &Output::Test {
                    path: &path,
                    filter,
                    opt_level: env.profile.opt_level,
                },
                &graph,
                &sources,
            );
            (path, job)
        })
        .collect();
    announce(
        env.ui,
        verb(jobs.iter().map(|(_, job)| job), "Compiling"),
        &package.manifest,
        &package.dir,
        &graph,
    );
    let mut code = 0;
    for (path, job) in &jobs {
        if !job.is_fresh() {
            run_job(job, env.ui)?;
        }
        env.ui.status("Running", &format!("`{}`", path.display()));
        let status = Command::new(path)
            .current_dir(&package.dir)
            .status()
            .map_err(|e| Error::from(format!("Could not run '{}': {}", path.display(), e)))?;
        if code == 0 {
            code = status.code().unwrap_or(1);
        }
    }
    Ok(code)
}

pub fn build(opts: Options, ui: Ui) -> Result<(), Error> {
    let start = Instant::now();
    let ws = discover()?;
    let env = Env::new(&ws, &opts, ui)?;
    sync_lock(&env, &ws, opts.locked)?;
    build_selected(&env, &ws, &opts)?;
    finished(ui, &env.profile.name, start);
    Ok(())
}

fn build_selected(env: &Env, ws: &Workspace, opts: &Options) -> Result<(), Error> {
    let mut notes = Vec::new();
    for package in ws.select(&opts.packages)? {
        notes.extend(build_roots(env, package)?);
    }
    for note in notes {
        println!("{}", note);
    }
    Ok(())
}

fn build_roots(env: &Env, package: &Package) -> Result<Option<String>, Error> {
    let roots = roots(&package.dir, &package.manifest)?;
    if roots.bin.is_some() {
        build_with(env, package)?;
    }
    let Some(lib) = roots.lib else {
        return Ok(None);
    };
    check_inputs(env, package, &[lib.as_path()])?;
    Ok(roots.bin.is_none().then(|| {
        format!(
            "No library artifact format yet; checked {}.",
            package.manifest.lib_path().display()
        )
    }))
}

pub fn run(opts: Options, program_args: &[String], ui: Ui) -> Result<i32, Error> {
    let ws = discover()?;
    let env = Env::new(&ws, &opts, ui)?;
    sync_lock(&env, &ws, opts.locked)?;
    let package = single_package(&ws, &opts.packages)?;
    run_with(&env, package, program_args)
}

fn run_with(env: &Env, package: &Package, program_args: &[String]) -> Result<i32, Error> {
    let start = Instant::now();
    let binary = build_with(env, package)?;
    finished(env.ui, &env.profile.name, start);
    env.ui.status("Running", &format!("`{}`", binary.display()));
    let status = Command::new(&binary)
        .args(program_args)
        .status()
        .map_err(|e| Error::from(format!("Could not run '{}': {}", binary.display(), e)))?;
    Ok(status.code().unwrap_or(1))
}

pub fn run_jit(opts: Options, program_args: &[String], ui: Ui) -> Result<i32, Error> {
    if let Some(err) = jit_host_error(env::consts::ARCH, env::consts::OS) {
        return Err(err);
    }
    let ws = discover()?;
    let env = Env::new(&ws, &opts, ui)?;
    sync_lock(&env, &ws, opts.locked)?;
    let package = single_package(&ws, &opts.packages)?;
    run_jit_with(&env, package, program_args)
}

fn jit_host_error(arch: &str, os: &str) -> Option<Error> {
    (arch != "x86_64" || os != "linux").then(|| {
        Error::with_help(
            format!("'run --jit' runs only on x86_64 Linux for now; this host is {arch} {os}."),
            "use 'phage run' instead",
        )
    })
}

fn run_jit_with(env: &Env, package: &Package, program_args: &[String]) -> Result<i32, Error> {
    let start = Instant::now();
    let bin = roots(&package.dir, &package.manifest)?
        .bin
        .ok_or_else(|| Error::from(format!("no binary root: {} not found", BIN_ROOT)))?;
    let out_dir = env.target.join("jit").join(package.manifest.name());
    make_dir(&out_dir)?;
    let graph = deps::collect(&package.dir, &package.manifest, &env.store)?;
    let sources = fingerprint::sources(&env.nukleus, &package.dir, &graph, &env.profile_key())?;
    let job = plan(
        env,
        &package.manifest.lints,
        &bin,
        &Output::Jit { out_dir: &out_dir },
        &graph,
        &sources,
    );
    announce(
        env.ui,
        verb([&job], "Compiling"),
        &package.manifest,
        &package.dir,
        &graph,
    );
    if !job.is_fresh() {
        run_job(&job, env.ui)?;
    }
    finished(env.ui, &env.profile.name, start);
    let ir = out_dir
        .join(bin.file_stem().unwrap_or_default())
        .with_extension("lamina");
    let (program, args) = jit_command(&env.lamina, &ir, env.profile.opt_level, program_args);
    env.ui.status(
        "Running",
        &format!("`{}`", ui::render_command(&program, &args)),
    );
    let status = Command::new(&program)
        .args(&args)
        .env("LAMINA_JIT_NO_FALLBACK", "1")
        .status()
        .map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                Error::with_help(
                    format!("lamina not found ('{}')", program.to_string_lossy()),
                    "put it on PATH or set LAMINA to its path",
                )
            } else {
                Error::from(format!(
                    "Could not run '{}': {}",
                    program.to_string_lossy(),
                    e
                ))
            }
        })?;
    match status.code().unwrap_or(1) {
        JIT_REFUSED_EXIT_CODE => Err(Error::with_help(
            "lamina could not JIT this program (its reason is above)",
            "use 'phage run' to build it ahead of time instead",
        )),
        code => Ok(code),
    }
}

fn jit_command(
    lamina: &OsStr,
    ir: &Path,
    opt_level: u8,
    program_args: &[String],
) -> (OsString, Vec<OsString>) {
    let mut args = vec![ir.as_os_str().to_os_string(), OsString::from("--jit")];
    if opt_level > 0 {
        args.push(OsString::from("--opt-level"));
        args.push(OsString::from(opt_level.to_string()));
    }
    if !program_args.is_empty() {
        args.push(OsString::from("--"));
        args.extend(program_args.iter().map(OsString::from));
    }
    (lamina.to_os_string(), args)
}

pub fn check(opts: Options, ui: Ui) -> Result<(), Error> {
    let start = Instant::now();
    let ws = discover()?;
    let env = Env::new(&ws, &opts, ui)?;
    sync_lock(&env, &ws, opts.locked)?;
    for package in ws.select(&opts.packages)? {
        check_with(&env, package)?;
    }
    finished(ui, &env.profile.name, start);
    Ok(())
}

fn check_with(env: &Env, package: &Package) -> Result<(), Error> {
    let roots = roots(&package.dir, &package.manifest)?;
    let inputs: Vec<&Path> = roots
        .bin
        .iter()
        .chain(roots.lib.iter())
        .map(PathBuf::as_path)
        .collect();
    check_inputs(env, package, &inputs)
}

fn check_inputs(env: &Env, package: &Package, inputs: &[&Path]) -> Result<(), Error> {
    let out_dir = env.target.join("check").join(package.manifest.name());
    make_dir(&out_dir)?;
    let graph = deps::collect(&package.dir, &package.manifest, &env.store)?;
    let sources = fingerprint::sources(&env.nukleus, &package.dir, &graph, &env.profile_key())?;
    let jobs: Vec<Job> = inputs
        .iter()
        .map(|input| {
            plan(
                env,
                &package.manifest.lints,
                input,
                &Output::Check { out_dir: &out_dir },
                &graph,
                &sources,
            )
        })
        .collect();
    if !jobs.is_empty() {
        announce(
            env.ui,
            verb(&jobs, "Checking"),
            &package.manifest,
            &package.dir,
            &graph,
        );
    }
    for job in &jobs {
        if !job.is_fresh() {
            run_job(job, env.ui)?;
        }
    }
    Ok(())
}

fn build_with(env: &Env, package: &Package) -> Result<PathBuf, Error> {
    let bin = roots(&package.dir, &package.manifest)?
        .bin
        .ok_or_else(|| Error::from(format!("no binary root: {} not found", BIN_ROOT)))?;
    let out_dir = env.target.join(&env.profile.dir);
    make_dir(&out_dir)?;
    let out = out_dir.join(package.manifest.name());
    let graph = deps::collect(&package.dir, &package.manifest, &env.store)?;
    let sources = fingerprint::sources(&env.nukleus, &package.dir, &graph, &env.profile_key())?;
    let job = plan(
        env,
        &package.manifest.lints,
        &bin,
        &Output::Binary {
            path: &out,
            opt_level: env.profile.opt_level,
        },
        &graph,
        &sources,
    );
    announce(
        env.ui,
        verb([&job], "Compiling"),
        &package.manifest,
        &package.dir,
        &graph,
    );
    if !job.is_fresh() {
        run_job(&job, env.ui)?;
    }
    Ok(out)
}

pub fn clean(ui: Ui) -> Result<(), Error> {
    let ws = discover()?;
    if remove_target(&ws.root)? {
        ui.status(
            "Removed",
            &format!("`{}`", ws.root.join("target").display()),
        );
    }
    Ok(())
}

/// Removes `<root>/target` only when the project's own manifest sits beside it.
fn remove_target(root: &Path) -> Result<bool, Error> {
    let target = root.join("target");
    if !target.is_dir() {
        return Ok(false);
    }
    let owned = target.file_name() == Some(OsStr::new("target")) && root.join(FILE_NAME).is_file();
    if !owned {
        return Err(Error::from(format!(
            "Refusing to remove '{}': no {} beside it",
            target.display(),
            FILE_NAME
        )));
    }
    fs::remove_dir_all(&target)
        .map_err(|e| Error::from(format!("Could not remove '{}': {}", target.display(), e)))?;
    Ok(true)
}

fn announce(ui: Ui, verb: &str, manifest: &Manifest, root: &Path, graph: &Graph) {
    ui.status(
        verb,
        &package_line(
            manifest.name(),
            manifest.version(),
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
        Source::Registry { index, .. } => format!("registry+{}", index),
    }
}

struct Job {
    program: OsString,
    args: Vec<OsString>,
    failure: &'static str,
    stamp: Stamp,
}

impl Job {
    fn is_fresh(&self) -> bool {
        self.stamp.is_fresh()
    }
}

fn plan(
    env: &Env,
    lints: &BTreeMap<String, LintLevel>,
    input: &Path,
    output: &Output,
    graph: &Graph,
    sources: &str,
) -> Job {
    let lints = profile::overlay(lints, &env.profile.lints);
    let (program, args) = compile_command(&CompileCommand {
        nukleus: &env.nukleus,
        input,
        output,
        graph,
        lints: &lints,
        profile_lints: &env.profile.lints,
    });
    let artifact = output.artifact(input);
    let stamp = Stamp::new(&artifact, sources, &args);
    Job {
        program,
        args,
        failure: output.failure(),
        stamp,
    }
}

fn run_job(job: &Job, ui: Ui) -> Result<(), Error> {
    job.stamp.clear()?;
    ui.verbose(
        "Running",
        &format!("`{}`", ui::render_command(&job.program, &job.args)),
    );
    let status = Command::new(&job.program)
        .args(&job.args)
        .status()
        .map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                Error::with_help(
                    format!("nukleus not found ('{}')", job.program.to_string_lossy()),
                    "put it on PATH or set NUKLEUS to its path",
                )
            } else {
                Error::from(format!(
                    "Could not run '{}': {}",
                    job.program.to_string_lossy(),
                    e
                ))
            }
        })?;
    if !status.success() {
        return Err(Error::from(job.failure));
    }
    job.stamp.write()
}

fn verb<'a>(jobs: impl IntoIterator<Item = &'a Job>, stale: &'static str) -> &'static str {
    if jobs.into_iter().all(Job::is_fresh) {
        "Fresh"
    } else {
        stale
    }
}

enum Output<'a> {
    Binary {
        path: &'a Path,
        opt_level: u8,
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
        opt_level: u8,
    },
}

impl Output<'_> {
    fn artifact(&self, input: &Path) -> PathBuf {
        match self {
            Output::Binary { path, .. } | Output::Test { path, .. } => path.to_path_buf(),
            Output::Check { out_dir } | Output::Jit { out_dir } => out_dir
                .join(input.file_stem().unwrap_or_default())
                .with_extension("lamina"),
        }
    }

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
    profile_lints: &'a BTreeMap<String, LintLevel>,
}

fn compile_command(cmd: &CompileCommand) -> (OsString, Vec<OsString>) {
    let mut args = Vec::new();
    match cmd.output {
        Output::Binary { path, opt_level } => {
            args.push(OsString::from("-o"));
            args.push(path.into());
            push_opt(&mut args, *opt_level);
        }
        Output::Check { out_dir } | Output::Jit { out_dir } => {
            args.push(OsString::from("--emit"));
            args.push(OsString::from("ir"));
            args.push(OsString::from("--out-dir"));
            args.push(out_dir.into());
        }
        Output::Test {
            path,
            filter,
            opt_level,
        } => {
            args.push(OsString::from("--test"));
            if let Some(filter) = filter {
                args.push(OsString::from("--test-filter"));
                args.push(OsString::from(filter));
            }
            args.push(OsString::from("-o"));
            args.push(path.into());
            push_opt(&mut args, *opt_level);
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
        gene_args(&mut args, gene, cmd.profile_lints);
    }
    args.push(cmd.input.into());
    (cmd.nukleus.to_os_string(), args)
}

fn push_opt(args: &mut Vec<OsString>, opt_level: u8) {
    if opt_level > 0 {
        args.push(OsString::from(format!("-O{}", opt_level)));
    }
}

fn gene_args(args: &mut Vec<OsString>, gene: &Gene, profile_lints: &BTreeMap<String, LintLevel>) {
    for (name, path) in &gene.deps {
        args.push(OsString::from("--gene-dep"));
        args.push(pair(&format!("{}:{}", gene.name, name), path.as_os_str()));
    }
    for (name, level) in &profile::overlay(&gene.lints, profile_lints) {
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
        opt_level: u8,
        graph: &'a Graph,
        lints: &'a BTreeMap<String, LintLevel>,
    ) -> (OsString, Vec<OsString>) {
        compile_command(&CompileCommand {
            nukleus: OsStr::new("nukleus"),
            input: Path::new("/p/src/main.nk"),
            output: &Output::Binary {
                path: out,
                opt_level,
            },
            graph,
            lints,
            profile_lints: &BTreeMap::new(),
        })
    }

    fn no_graph() -> Graph {
        Graph {
            deps: Vec::new(),
            genes: Vec::new(),
        }
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
            profile_lints: &BTreeMap::new(),
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
            profile_lints: &BTreeMap::new(),
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
            0,
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
    fn jit_command_optimized_asks_lamina_for_that_level() {
        let (_, args) = jit_command(
            OsStr::new("lamina"),
            Path::new("/p/target/jit/main.lamina"),
            2,
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
            0,
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
        assert!(jit_host_error("x86_64", "linux").is_none());
        let err = jit_host_error("aarch64", "linux").unwrap();
        assert!(
            err.contains("x86_64 Linux") && err.contains("aarch64 linux"),
            "{err}"
        );
        assert!(jit_host_error("x86_64", "macos").is_some());
    }

    #[test]
    fn compile_command_test_builds_runner_with_filter() {
        let (program, args) = compile_command(&CompileCommand {
            nukleus: OsStr::new("nukleus"),
            input: Path::new("/p/src/main.nk"),
            output: &Output::Test {
                path: Path::new("/p/target/test/demo"),
                filter: Some("geom"),
                opt_level: 0,
            },
            graph: &no_graph(),
            lints: &BTreeMap::new(),
            profile_lints: &BTreeMap::new(),
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
            0,
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
            3,
            &no_graph(),
            &BTreeMap::new(),
        );
        assert_eq!(
            args,
            vec![
                OsString::from("-o"),
                OsString::from("/p/target/release/demo"),
                OsString::from("-O3"),
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
        let (_, args) = command(Path::new("/p/target/debug/demo"), 0, &graph, &lints);
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
    fn profile_lints_override_gene_lints() {
        let graph = Graph {
            deps: Vec::new(),
            genes: vec![Gene {
                name: "a".to_string(),
                version: "0.1.0".to_string(),
                root: PathBuf::from("/p/a/src/lib.nk"),
                deps: Vec::new(),
                lints: BTreeMap::from([("never_mutated".to_string(), LintLevel::Warn)]),
                dir: PathBuf::from("/p/a"),
                source: Source::Path,
            }],
        };
        let profile = BTreeMap::from([("never_mutated".to_string(), LintLevel::Deny)]);
        let (_, args) = compile_command(&CompileCommand {
            nukleus: OsStr::new("nukleus"),
            input: Path::new("/p/src/main.nk"),
            output: &Output::Check {
                out_dir: Path::new("/p/target/check"),
            },
            graph: &graph,
            lints: &BTreeMap::new(),
            profile_lints: &profile,
        });
        assert!(
            args.contains(&OsString::from("a:never_mutated=deny")),
            "{args:?}"
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
                    source: Source::Path,
                },
                Gene {
                    name: "b".to_string(),
                    version: "0.1.0".to_string(),
                    root: PathBuf::from("/p/b/src/lib.nk"),
                    deps: Vec::new(),
                    lints: BTreeMap::new(),
                    dir: PathBuf::from("/p/b"),
                    source: Source::Path,
                },
            ],
        };
        let root_lints = BTreeMap::from([("never_mutated".to_string(), LintLevel::Warn)]);
        let (_, args) = command(Path::new("/p/target/debug/demo"), 0, &graph, &root_lints);
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

    #[test]
    fn clean_removes_target_beside_manifest() -> io::Result<()> {
        use crate::testutil::temp_dir;
        let root = temp_dir("clean_ok")?;
        fs::write(root.join(FILE_NAME), "")?;
        fs::create_dir_all(root.join("target/debug"))?;
        let removed = remove_target(&root).map_err(io::Error::other);
        let gone = !root.join("target").exists();
        fs::remove_dir_all(&root)?;
        assert!(removed?);
        assert!(gone);
        Ok(())
    }

    #[test]
    fn clean_refuses_target_without_manifest_beside_it() -> io::Result<()> {
        use crate::testutil::temp_dir;
        let root = temp_dir("clean_refuse")?;
        fs::create_dir_all(root.join("target"))?;
        let err = remove_target(&root).unwrap_err();
        let kept = root.join("target").is_dir();
        fs::remove_dir_all(&root)?;
        assert!(err.contains("Refusing"), "{err}");
        assert!(kept);
        Ok(())
    }

    #[test]
    fn clean_without_target_removes_nothing() -> io::Result<()> {
        use crate::testutil::temp_dir;
        let root = temp_dir("clean_none")?;
        fs::write(root.join(FILE_NAME), "")?;
        let removed = remove_target(&root).map_err(io::Error::other);
        fs::remove_dir_all(&root)?;
        assert!(!removed?);
        Ok(())
    }

    #[test]
    fn release_conflicts_with_a_different_profile() {
        let opts = Options {
            release: true,
            profile: Some("fast".to_string()),
            ..Default::default()
        };
        assert!(profile_name(&opts).is_err());
        let plain = Options {
            release: true,
            ..Default::default()
        };
        assert_eq!(profile_name(&plain).ok(), Some("release"));
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
                project: Some(Project {
                    name: "demo".to_string(),
                    version: "0.1.0".to_string(),
                }),
                workspace: None,
                lib: None,
                dependencies: BTreeMap::new(),
                lints: BTreeMap::new(),
                profile: BTreeMap::new(),
            }
        }

        fn package(root: &Path) -> Package {
            Package {
                dir: root.to_path_buf(),
                manifest: demo(),
            }
        }

        fn env(nukleus: &Path, root: &Path, profile: &str) -> Env {
            Env {
                nukleus: nukleus.as_os_str().to_os_string(),
                lamina: OsString::from("lamina"),
                store: store(root),
                target: root.join("target"),
                profile: Profile::resolve(profile, &BTreeMap::new()).unwrap(),
                ui: Ui::default(),
            }
        }

        fn lib_only(tag: &str) -> io::Result<PathBuf> {
            let root = project(tag)?;
            fs::remove_file(root.join("src/main.nk"))?;
            fs::write(root.join("src/lib.nk"), "")?;
            Ok(root)
        }

        #[test]
        fn build_creates_debug_binary() -> io::Result<()> {
            let _guard = lock();
            let root = project("build")?;
            let nukleus = root.join("fake-nukleus");
            write_exec(&nukleus, FAKE_NUKLEUS)?;

            let out = build_with(&env(&nukleus, &root, "dev"), &package(&root))
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

            let err = build_with(&env(&nukleus, &root, "release"), &package(&root)).unwrap_err();
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

            let code = run_with(&env(&nukleus, &root, "release"), &package(&root), &[])
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

            let err = build_with(&env(&nukleus, &root, "dev"), &package(&root)).unwrap_err();
            assert!(
                err.help.as_deref().is_some_and(|h| h.contains("NUKLEUS")),
                "{err}"
            );
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

        #[test]
        fn check_lib_only_checks_lib_root() -> io::Result<()> {
            let _guard = lock();
            let root = lib_only("check_lib")?;
            let nukleus = root.join("fake-nukleus");
            write_exec(&nukleus, FAKE_CHECK)?;

            check_with(&env(&nukleus, &root, "dev"), &package(&root)).map_err(io::Error::other)?;
            let written = root.join("target/check/demo/lib.lamina").is_file();
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

            check_with(&env(&nukleus, &root, "dev"), &package(&root)).map_err(io::Error::other)?;
            let written = root.join("target/check/demo/main.lamina").is_file()
                && root.join("target/check/demo/lib.lamina").is_file();
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

            let note = build_roots(&env(&nukleus, &root, "dev"), &package(&root))
                .map_err(io::Error::other)?;
            let checked = root.join("target/check/demo/lib.lamina").is_file();
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

            let note = build_roots(&env(&nukleus, &root, "dev"), &package(&root))
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

            let err = run_with(&env(&nukleus, &root, "dev"), &package(&root), &[]).unwrap_err();
            let checked = root.join("target/check/demo/lib.lamina").exists();
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

            check_with(&env(&nukleus, &root, "dev"), &package(&root)).map_err(io::Error::other)?;
            assert!(root.join("target/check/demo/main.lamina").is_file());
            fs::remove_dir_all(&root)?;
            Ok(())
        }

        #[test]
        fn test_runs_runner_and_returns_its_exit_code() -> io::Result<()> {
            let _guard = lock();
            let root = project("test_run")?;
            let nukleus = root.join("fake-nukleus");
            write_exec(&nukleus, FAKE_NUKLEUS)?;

            let code = test_with(&env(&nukleus, &root, "dev"), &package(&root), None)
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

            let code = test_with(&env(&nukleus, &root, "dev"), &package(&root), Some("x"))
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

            let err = test_with(&env(&nukleus, &root, "dev"), &package(&root), None).unwrap_err();
            assert_eq!(err, "test build failed");
            Ok(())
        }

        #[test]
        fn check_reports_nukleus_error() -> io::Result<()> {
            let _guard = lock();
            let root = project("check_fail")?;
            let nukleus = root.join("fake-nukleus");
            write_exec(&nukleus, "#!/bin/sh\nexit 1\n")?;

            let err = check_with(&env(&nukleus, &root, "dev"), &package(&root)).unwrap_err();
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

        fn jit_env(nukleus: &Path, lamina: &Path, root: &Path) -> Env {
            Env {
                lamina: lamina.as_os_str().to_os_string(),
                ..env(nukleus, root, "dev")
            }
        }

        #[test]
        fn run_jit_builds_ir_and_returns_lamina_exit_code() -> io::Result<()> {
            let _guard = lock();
            let root = project("jit")?;
            let nukleus = root.join("fake-nukleus");
            let lamina = root.join("fake-lamina");
            write_exec(&nukleus, FAKE_CHECK)?;
            write_exec(&lamina, FAKE_LAMINA)?;

            let code = run_jit_with(&jit_env(&nukleus, &lamina, &root), &package(&root), &[])
                .map_err(io::Error::other)?;
            assert_eq!(code, 7);
            assert!(root.join("target/jit/demo/main.lamina").is_file());
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
                &jit_env(&nukleus, &lamina, &root),
                &package(&root),
                &["a".to_string(), "b c".to_string()],
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

            let err =
                run_jit_with(&jit_env(&nukleus, &lamina, &root), &package(&root), &[]).unwrap_err();
            assert!(
                err.help.as_deref().is_some_and(|h| h.contains("phage run")),
                "{err}"
            );
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

            let code = run_jit_with(&jit_env(&nukleus, &lamina, &root), &package(&root), &[])
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

            let err =
                run_jit_with(&jit_env(&nukleus, &lamina, &root), &package(&root), &[]).unwrap_err();
            assert!(
                err.help.as_deref().is_some_and(|h| h.contains("LAMINA")),
                "{err}"
            );
            Ok(())
        }

        const COUNTING_NUKLEUS: &str = "#!/bin/sh
if [ \"$1\" = \"--version\" ]; then echo fake-1; exit 0; fi
echo run >> \"LOG\"
dir=\"\"
out=\"\"
last=\"\"
while [ $# -gt 0 ]; do
  if [ \"$1\" = \"--out-dir\" ]; then dir=\"$2\"; shift; fi
  if [ \"$1\" = \"-o\" ]; then out=\"$2\"; shift; fi
  last=\"$1\"
  shift
done
if [ -n \"$dir\" ]; then
  mkdir -p \"$dir\" && echo ir > \"$dir/$(basename \"$last\" .nk).lamina\"
else
  printf '#!/bin/sh\\necho built\\nexit 0\\n' > \"$out\"
  chmod +x \"$out\"
fi
";

        fn counting_setup(tag: &str) -> io::Result<(PathBuf, PathBuf, PathBuf)> {
            let root = project(tag)?;
            let log = root.join("invocations");
            let nukleus = root.join("fake-nukleus");
            let script = COUNTING_NUKLEUS.replace("LOG", &log.display().to_string());
            write_exec(&nukleus, &script)?;
            Ok((root, nukleus, log))
        }

        fn invocations(log: &Path) -> usize {
            fs::read_to_string(log).map_or(0, |text| text.lines().count())
        }

        #[test]
        fn build_skips_nukleus_when_fresh() -> io::Result<()> {
            let _guard = lock();
            let (root, nukleus, log) = counting_setup("fresh")?;
            build_with(&env(&nukleus, &root, "dev"), &package(&root)).map_err(io::Error::other)?;
            build_with(&env(&nukleus, &root, "dev"), &package(&root)).map_err(io::Error::other)?;
            let stamped = root.join("target/debug/.fingerprint/demo").is_file();
            let runs = invocations(&log);
            fs::remove_dir_all(&root)?;
            assert_eq!(runs, 1);
            assert!(stamped);
            Ok(())
        }

        #[test]
        fn build_reruns_after_source_edit() -> io::Result<()> {
            let _guard = lock();
            let (root, nukleus, log) = counting_setup("edit")?;
            build_with(&env(&nukleus, &root, "dev"), &package(&root)).map_err(io::Error::other)?;
            fs::write(root.join("src/main.nk"), "changed")?;
            build_with(&env(&nukleus, &root, "dev"), &package(&root)).map_err(io::Error::other)?;
            let runs = invocations(&log);
            fs::remove_dir_all(&root)?;
            assert_eq!(runs, 2);
            Ok(())
        }

        #[test]
        fn build_reruns_when_output_is_missing() -> io::Result<()> {
            let _guard = lock();
            let (root, nukleus, log) = counting_setup("missing_out")?;
            build_with(&env(&nukleus, &root, "dev"), &package(&root)).map_err(io::Error::other)?;
            fs::remove_file(root.join("target/debug/demo"))?;
            build_with(&env(&nukleus, &root, "dev"), &package(&root)).map_err(io::Error::other)?;
            let runs = invocations(&log);
            fs::remove_dir_all(&root)?;
            assert_eq!(runs, 2);
            Ok(())
        }

        #[test]
        fn build_reruns_when_profile_changes_optimization() -> io::Result<()> {
            let _guard = lock();
            let (root, nukleus, log) = counting_setup("profile")?;
            build_with(&env(&nukleus, &root, "dev"), &package(&root)).map_err(io::Error::other)?;
            build_with(&env(&nukleus, &root, "release"), &package(&root))
                .map_err(io::Error::other)?;
            let runs = invocations(&log);
            let optimized = root.join("target/release/demo").is_file();
            fs::remove_dir_all(&root)?;
            assert_eq!(runs, 2);
            assert!(optimized);
            Ok(())
        }

        #[test]
        fn build_reruns_when_lints_change() -> io::Result<()> {
            let _guard = lock();
            let (root, nukleus, log) = counting_setup("lints")?;
            build_with(&env(&nukleus, &root, "dev"), &package(&root)).map_err(io::Error::other)?;
            let mut linted = package(&root);
            linted
                .manifest
                .lints
                .insert("never_mutated".to_string(), LintLevel::Warn);
            build_with(&env(&nukleus, &root, "dev"), &linted).map_err(io::Error::other)?;
            let runs = invocations(&log);
            fs::remove_dir_all(&root)?;
            assert_eq!(runs, 2);
            Ok(())
        }

        #[test]
        fn check_skips_nukleus_when_fresh() -> io::Result<()> {
            let _guard = lock();
            let (root, nukleus, log) = counting_setup("check_fresh")?;
            for _ in 0..2 {
                check_with(&env(&nukleus, &root, "dev"), &package(&root))
                    .map_err(io::Error::other)?;
            }
            let runs = invocations(&log);
            fs::remove_dir_all(&root)?;
            assert_eq!(runs, 1);
            Ok(())
        }

        #[test]
        fn workspace_builds_every_member_into_one_target() -> io::Result<()> {
            let _guard = lock();
            let base = temp_dir("ws_build")?;
            fs::write(
                base.join("Phage.toml"),
                "[workspace]\nmembers = [\"a\", \"b/*\"]\n",
            )?;
            for dir in ["a", "b/c"] {
                let member = base.join(dir);
                fs::create_dir_all(member.join("src"))?;
                fs::write(member.join("src/main.nk"), "")?;
                let name = dir.replace('/', "-");
                fs::write(
                    member.join(FILE_NAME),
                    format!("[project]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
                )?;
            }
            let nukleus = base.join("fake-nukleus");
            write_exec(&nukleus, FAKE_NUKLEUS)?;

            let ws = Workspace::discover(&base).map_err(io::Error::other)?;
            let env = Env {
                nukleus: nukleus.as_os_str().to_os_string(),
                lamina: OsString::from("lamina"),
                store: store(&base),
                target: ws.root.join("target"),
                profile: Profile::resolve("dev", &BTreeMap::new()).map_err(io::Error::other)?,
                ui: Ui::default(),
            };
            let opts = Options::default();
            sync_lock(&env, &ws, false).map_err(io::Error::other)?;
            build_selected(&env, &ws, &opts).map_err(io::Error::other)?;
            let both =
                base.join("target/debug/a").is_file() && base.join("target/debug/b-c").is_file();
            let lock_at_root = base.join(lock::FILE_NAME).is_file();
            fs::remove_dir_all(&base)?;
            assert!(both);
            assert!(lock_at_root);
            Ok(())
        }
    }
}
