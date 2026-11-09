use crate::manifest::{self, FILE_NAME, find_manifest};
use std::{
    env,
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

struct Target {
    nukleus: OsString,
    root: PathBuf,
    name: String,
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
        name: manifest.project.name,
    })
}

pub fn build(release: bool) -> Result<PathBuf, String> {
    let target = resolve()?;
    build_with(&target.nukleus, &target.root, &target.name, release)
}

pub fn run(release: bool, program_args: &[String]) -> Result<i32, String> {
    let target = resolve()?;
    run_with(
        &target.nukleus,
        &target.root,
        &target.name,
        release,
        program_args,
    )
}

fn run_with(
    nukleus: &OsStr,
    root: &Path,
    name: &str,
    release: bool,
    program_args: &[String],
) -> Result<i32, String> {
    let binary = build_with(nukleus, root, name, release)?;
    let status = Command::new(&binary)
        .args(program_args)
        .status()
        .map_err(|e| format!("Could not run '{}': {}", binary.display(), e))?;
    Ok(status.code().unwrap_or(1))
}

fn build_with(nukleus: &OsStr, root: &Path, name: &str, release: bool) -> Result<PathBuf, String> {
    let profile = if release { "release" } else { "debug" };
    let out_dir = root.join("target").join(profile);
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("Could not create '{}': {}", out_dir.display(), e))?;
    let out = out_dir.join(name);
    let (program, args) = compile_command(nukleus, &root.join("src/main.nk"), &out, release);
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

fn compile_command(
    nukleus: &OsStr,
    input: &Path,
    out: &Path,
    release: bool,
) -> (OsString, Vec<OsString>) {
    let mut args = vec![OsString::from("-o"), out.into()];
    if release {
        args.push(OsString::from("-O2"));
    }
    args.push(input.into());
    (nukleus.to_os_string(), args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_command_debug() {
        let (program, args) = compile_command(
            OsStr::new("nukleus"),
            Path::new("/p/src/main.nk"),
            Path::new("/p/target/debug/demo"),
            false,
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
        let (_, args) = compile_command(
            OsStr::new("nukleus"),
            Path::new("/p/src/main.nk"),
            Path::new("/p/target/release/demo"),
            true,
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

    #[cfg(unix)]
    mod fake {
        use super::*;
        use crate::testutil::temp_dir;
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

        #[test]
        fn build_creates_debug_binary() -> io::Result<()> {
            let _guard = lock();
            let root = project("build")?;
            let nukleus = root.join("fake-nukleus");
            write_exec(&nukleus, FAKE_NUKLEUS)?;

            let out =
                build_with(nukleus.as_os_str(), &root, "demo", false).map_err(io::Error::other)?;
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

            let err = build_with(nukleus.as_os_str(), &root, "demo", true).unwrap_err();
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

            let code = run_with(nukleus.as_os_str(), &root, "demo", true, &[])
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

            let err = build_with(nukleus.as_os_str(), &root, "demo", false).unwrap_err();
            assert!(err.contains("NUKLEUS"), "{err}");
            fs::remove_dir_all(&root)?;
            Ok(())
        }
    }
}
