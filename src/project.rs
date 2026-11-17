use crate::error::Error;
use crate::manifest::{self, FILE_NAME};
use std::{
    fs,
    io::{self, ErrorKind},
    path::Path,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bin,
    Lib,
}

impl Kind {
    fn root(self) -> &'static str {
        match self {
            Kind::Bin => "src/main.nk",
            Kind::Lib => "src/lib.nk",
        }
    }

    fn source(self) -> &'static str {
        match self {
            Kind::Bin => "fn main() -> i32 {\n    return 0;\n}\n",
            Kind::Lib => "fn answer() -> i32 {\n    return 42;\n}\n",
        }
    }
}

pub fn init(path: &Path, kind: Kind) -> io::Result<()> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(
                ErrorKind::InvalidInput,
                "project path has no UTF-8 file name",
            )
        })?;
    let text = manifest::template(name).map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
    fs::create_dir(path.join("src"))?;
    fs::write(path.join(kind.root()), kind.source())?;
    fs::write(path.join(FILE_NAME), text.as_bytes())?;

    Ok(())
}

pub fn new(dir: &str, kind: Kind) -> Result<(), Error> {
    let path = Path::new(dir);
    fs::create_dir(path).map_err(|e| {
        if e.kind() == ErrorKind::AlreadyExists {
            format!("Directory '{}' already exists.", dir)
        } else {
            format!("Could not create directory '{}': {}", dir, e)
        }
    })?;
    init(path, kind).map_err(|e| Error::from(format!("Could not create project: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::temp_dir;

    #[test]
    fn phage_toml_is_valid_toml() -> io::Result<()> {
        let base = temp_dir("init")?;
        let project = base.join("demo");
        fs::create_dir(&project)?;

        init(&project, Kind::Bin)?;
        let text = fs::read_to_string(project.join(FILE_NAME))?;
        fs::remove_dir_all(&base)?;

        assert_eq!(
            text,
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\n"
        );
        Ok(())
    }

    #[test]
    fn lib_scaffold_writes_lib_root_only() -> io::Result<()> {
        let base = temp_dir("lib")?;
        let project = base.join("demo");
        new(&project.to_string_lossy(), Kind::Lib).map_err(io::Error::other)?;

        let lib = fs::read_to_string(project.join("src/lib.nk"))?;
        let has_main = project.join("src/main.nk").exists();
        fs::remove_dir_all(&base)?;

        assert!(lib.contains("fn answer"), "{lib}");
        assert!(!has_main);
        Ok(())
    }

    #[test]
    fn bin_scaffold_writes_main_root_only() -> io::Result<()> {
        let base = temp_dir("bin")?;
        let project = base.join("demo");
        new(&project.to_string_lossy(), Kind::Bin).map_err(io::Error::other)?;

        let main = fs::read_to_string(project.join("src/main.nk"))?;
        let has_lib = project.join("src/lib.nk").exists();
        fs::remove_dir_all(&base)?;

        assert!(main.contains("fn main"), "{main}");
        assert!(!has_lib);
        Ok(())
    }

    #[test]
    fn new_refuses_existing_directory() -> io::Result<()> {
        let base = temp_dir("exists")?;
        let project = base.join("demo");
        fs::create_dir(&project)?;

        let err = new(&project.to_string_lossy(), Kind::Bin).unwrap_err();
        fs::remove_dir_all(&base)?;
        assert!(err.contains("already exists"), "{err}");
        Ok(())
    }
}
