use crate::{
    deps,
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

pub fn build(release: bool) -> Result<PathBuf, String> {
    let target = resolve()?;
    build_with(&target.nukleus, &target.root, &target.manifest, release)
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

fn build_with(
    nukleus: &OsStr,
    root: &Path,
    manifest: &Manifest,
    release: bool,
) -> Result<PathBuf, String> {
    let profile = if release { "release" } else { "debug" };
    let out_dir = root.join("target").join(profile);
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("Could not create '{}': {}", out_dir.display(), e))?;
    let out = out_dir.join(&manifest.project.name);
    let genes = deps::collect(root, manifest)?;
    let (program, args) = compile_command(&CompileCommand {
        nukleus,
        input: &root.join("src/main.nk"),
        out: &out,
        release,
        deps: &genes,
        lints: &manifest.lints,
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
        return Err("build failed".to_string());
    }
    Ok(out)
}

struct CompileCommand<'a> {
    nukleus: &'a OsStr,
    input: &'a Path,
    out: &'a Path,
    release: bool,
    deps: &'a [(String, PathBuf)],
    lints: &'a BTreeMap<String, LintLevel>,
}

fn compile_command(cmd: &CompileCommand) -> (OsString, Vec<OsString>) {
    let mut args = vec![OsString::from("-o"), cmd.out.into()];
    if cmd.release {
        args.push(OsString::from("-O2"));
    }
    for (name, path) in cmd.deps {
        args.push(OsString::from("--dep"));
        args.push(pair(name, path.as_os_str()));
    }
    for (name, level) in cmd.lints {
        args.push(OsString::from("--lint"));
        args.push(pair(name, OsStr::new(level.as_str())));
    }
    args.push(cmd.input.into());
    (cmd.nukleus.to_os_string(), args)
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
        deps: &'a [(String, PathBuf)],
        lints: &'a BTreeMap<String, LintLevel>,
    ) -> (OsString, Vec<OsString>) {
        compile_command(&CompileCommand {
            nukleus: OsStr::new("nukleus"),
            input: Path::new("/p/src/main.nk"),
            out,
            release,
            deps,
            lints,
        })
    }

    #[test]
    fn compile_command_debug() {
        let (program, args) = command(
            Path::new("/p/target/debug/demo"),
            false,
            &[],
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
            &[],
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
        let deps = [("json".to_string(), PathBuf::from("/p/json/src/lib.nk"))];
        let lints = BTreeMap::from([
            ("never_mutated".to_string(), LintLevel::Warn),
            ("future_lint".to_string(), LintLevel::Deny),
        ]);
        let (_, args) = command(Path::new("/p/target/debug/demo"), false, &deps, &lints);
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
    }
}
