use crate::{
    deps::{self, Gene, Graph},
    manifest::{self, FILE_NAME, LintLevel, Manifest, find_manifest},
};
use std::{
    collections::BTreeMap,
    env,
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

struct Target {
    nukleus: OsString,
    root: PathBuf,
    manifest: Manifest,
}

fn resolve() -> Result<Target, String> {
    let cwd =
        env::current_dir().map_err(|e| format!("Could not read the current directory: {}", e))?;
    let manifest_path = find_manifest(&cwd).ok_or_else(|| {
        format!(
            "No {} found in '{}' or any parent directory.",
            FILE_NAME,
            cwd.display()
        )
    })?;
    let root = manifest_path
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let manifest = manifest::load(&manifest_path)?;
    Ok(Target {
        nukleus: env::var_os("NUKLEUS").unwrap_or_else(|| OsString::from("nukleus")),
        root,
        manifest,
    })
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

pub fn build(release: bool) -> Result<(), String> {
    let target = resolve()?;
    if let Some(note) = build_roots(&target.nukleus, &target.root, &target.manifest, release)? {
        println!("{}", note);
    }
    Ok(())
}

fn build_roots(
    nukleus: &OsStr,
    root: &Path,
    manifest: &Manifest,
    release: bool,
) -> Result<Option<String>, String> {
    let roots = roots(root, manifest)?;
    if roots.bin.is_some() {
        build_with(nukleus, root, manifest, release)?;
    }
    let Some(lib) = roots.lib else {
        return Ok(None);
    };
    check_inputs(nukleus, root, manifest, &[lib.as_path()])?;
    Ok(roots.bin.is_none().then(|| {
        format!(
            "No library artifact format yet; checked {}.",
            manifest.lib_path().display()
        )
    }))
}

pub fn run(release: bool, program_args: &[String]) -> Result<i32, String> {
    let target = resolve()?;
    run_with(
        &target.nukleus,
        &target.root,
        &target.manifest,
        release,
        program_args,
    )
}

fn run_with(
    nukleus: &OsStr,
    root: &Path,
    manifest: &Manifest,
    release: bool,
    program_args: &[String],
) -> Result<i32, String> {
    let binary = build_with(nukleus, root, manifest, release)?;
    let status = Command::new(&binary)
        .args(program_args)
        .status()
        .map_err(|e| format!("Could not run '{}': {}", binary.display(), e))?;
    Ok(status.code().unwrap_or(1))
}

pub fn check() -> Result<(), String> {
    let target = resolve()?;
    check_with(&target.nukleus, &target.root, &target.manifest)
}

fn check_with(nukleus: &OsStr, root: &Path, manifest: &Manifest) -> Result<(), String> {
    let roots = roots(root, manifest)?;
    let inputs: Vec<&Path> = roots
        .bin
        .iter()
        .chain(roots.lib.iter())
        .map(PathBuf::as_path)
        .collect();
    check_inputs(nukleus, root, manifest, &inputs)
}

fn check_inputs(
    nukleus: &OsStr,
    root: &Path,
    manifest: &Manifest,
    inputs: &[&Path],
) -> Result<(), String> {
    let out_dir = root.join("target").join("check");
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("Could not create '{}': {}", out_dir.display(), e))?;
    let graph = deps::collect(root, manifest)?;
    for input in inputs {
        compile(
            nukleus,
            input,
            &Output::Check { out_dir: &out_dir },
            &graph,
            &manifest.lints,
        )?;
    }
    Ok(())
}

fn build_with(
    nukleus: &OsStr,
    root: &Path,
    manifest: &Manifest,
    release: bool,
) -> Result<PathBuf, String> {
    let bin = roots(root, manifest)?
        .bin
        .ok_or_else(|| format!("no binary root: {} not found", BIN_ROOT))?;
    let profile = if release { "release" } else { "debug" };
    let out_dir = root.join("target").join(profile);
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("Could not create '{}': {}", out_dir.display(), e))?;
    let out = out_dir.join(&manifest.project.name);
    let graph = deps::collect(root, manifest)?;
    compile(
        nukleus,
        &bin,
        &Output::Binary {
            path: &out,
            release,
        },
        &graph,
        &manifest.lints,
    )?;
    Ok(out)
}

fn compile(
    nukleus: &OsStr,
    input: &Path,
    output: &Output,
    graph: &Graph,
    lints: &BTreeMap<String, LintLevel>,
) -> Result<(), String> {
    let (program, args) = compile_command(&CompileCommand {
        nukleus,
        input,
        output,
        graph,
        lints,
    });
    let status = Command::new(&program).args(&args).status().map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            format!(
                "nukleus not found ('{}'): put it on PATH or set NUKLEUS to its path",
                program.to_string_lossy()
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
    Binary { path: &'a Path, release: bool },
    Check { out_dir: &'a Path },
}

impl Output<'_> {
    fn failure(&self) -> &'static str {
        match self {
            Output::Binary { .. } => "build failed",
            Output::Check { .. } => "check failed",
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
        Output::Check { out_dir } => {
            args.push(OsString::from("--emit"));
            args.push(OsString::from("ir"));
            args.push(OsString::from("--out-dir"));
            args.push(out_dir.into());
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

    fn no_graph() -> Graph {
        Graph {
            deps: Vec::new(),
            genes: Vec::new(),
        }
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
                    root: PathBuf::from("/p/a/src/lib.nk"),
                    deps: vec![("b".to_string(), PathBuf::from("/p/b/src/lib.nk"))],
                    lints: BTreeMap::from([("never_mutated".to_string(), LintLevel::Deny)]),
                    dir: PathBuf::from("/p/a"),
                },
                Gene {
                    name: "b".to_string(),
                    root: PathBuf::from("/p/b/src/lib.nk"),
                    deps: Vec::new(),
                    lints: BTreeMap::new(),
                    dir: PathBuf::from("/p/b"),
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

            let out =
                build_with(nukleus.as_os_str(), &root, &demo(), false).map_err(io::Error::other)?;
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

            let err = build_with(nukleus.as_os_str(), &root, &demo(), true).unwrap_err();
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

            let code = run_with(nukleus.as_os_str(), &root, &demo(), true, &[])
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

            let err = build_with(nukleus.as_os_str(), &root, &demo(), false).unwrap_err();
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

            check_with(nukleus.as_os_str(), &root, &demo()).map_err(io::Error::other)?;
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

            check_with(nukleus.as_os_str(), &root, &demo()).map_err(io::Error::other)?;
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

            let note = build_roots(nukleus.as_os_str(), &root, &demo(), false)
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

            let note = build_roots(nukleus.as_os_str(), &root, &demo(), false)
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

            let err = run_with(nukleus.as_os_str(), &root, &demo(), false, &[]).unwrap_err();
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

            check_with(nukleus.as_os_str(), &root, &demo()).map_err(io::Error::other)?;
            assert!(root.join("target/check/main.lamina").is_file());
            fs::remove_dir_all(&root)?;
            Ok(())
        }

        #[test]
        fn check_reports_nukleus_error() -> io::Result<()> {
            let _guard = lock();
            let root = project("check_fail")?;
            let nukleus = root.join("fake-nukleus");
            write_exec(&nukleus, "#!/bin/sh\nexit 1\n")?;

            let err = check_with(nukleus.as_os_str(), &root, &demo()).unwrap_err();
            assert_eq!(err, "check failed");
            fs::remove_dir_all(&root)?;
            Ok(())
        }
    }
}
