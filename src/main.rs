use inksac::{Color, ColoredString, Style};
use std::{
    env, fs,
    io::{self, ErrorKind, Result},
    path::Path,
    process,
};

// existing Phage and Package structs and impls omitted for brevity...
const ERRORSTYLE: Style = Style {
    foreground: Color::Red,
    background: Color::Empty,
    bold: false,
    dim: false,
    italic: false,
    underline: false,
};

fn fail(msg: &str) -> ! {
    eprintln!("{}", ColoredString::new(msg, ERRORSTYLE));
    process::exit(1);
}

fn main() {
    // let mut phage = Phage::new();

    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        print_help_main();
        process::exit(1);
    }

    match args[1].as_str() {
        "-v" | "--version" => {
            println!("Phage version 0.1.0");
        }
        "-h" | "--help" => {
            print_help_main();
        }
        "init" => {
            let path = env::current_dir()
                .unwrap_or_else(|e| fail(&format!("Could not read the current directory: {}", e)));
            create_project_files(&path)
                .unwrap_or_else(|e| fail(&format!("Could not create project: {}", e)));
        }
        "new" => {
            if args.len() < 3 {
                println!("Please provide a project name.");
                process::exit(1);
            }

            let dir = &args[2];
            if let Err(e) = fs::create_dir(dir) {
                if e.kind() == ErrorKind::AlreadyExists {
                    fail(&format!("Directory '{}' already exists.", dir));
                }
                fail(&format!("Could not create directory '{}': {}", dir, e));
            }
            create_project_files(Path::new(dir))
                .unwrap_or_else(|e| fail(&format!("Could not create project: {}", e)));
        }
        "install" => {
            // previous install code omitted for brevity...
        }
        // other commands omitted for brevity...
        _ => {
            println!("{}", ColoredString::new("Invalid command.", ERRORSTYLE));
            process::exit(1);
        }
    }
}
fn print_help_main() {
    let title_style = Style {
        foreground: Color::Cyan,
        ..Default::default()
    };
    let h2_style = Style {
        foreground: Color::Yellow,
        ..Default::default()
    };
    let h3_style = Style {
        foreground: Color::Green,
        ..Default::default()
    };
    let _test_style = Style {
        foreground: Color::RGB(102, 103, 171),
        ..Default::default()
    };
    println!(
        "{}, a package and build manager for Nukleus",
        ColoredString::new("Phage", title_style)
    );
    println!("\n{}:", ColoredString::new("Commands", h2_style));
    println!(
        "  {}               Create a new project in the current directory",
        ColoredString::new("init", h3_style)
    );
    println!(
        "  {}                Create a new project in a new directory",
        ColoredString::new("new", h3_style)
    );
    println!(
        "  {}             Inject a gene",
        ColoredString::new("inject", h3_style)
    );
    println!(
        "  {}             Remove a gene",
        ColoredString::new("remove", h3_style)
    );
    println!(
        "  {}              Build the current project",
        ColoredString::new("build", h3_style)
    );
    println!(
        "  {}                Run the current project",
        ColoredString::new("run", h3_style)
    );
    println!(
        "  {}               List installed genes",
        ColoredString::new("list", h3_style)
    );
    println!(
        "  {}        Print this message",
        ColoredString::new("-h, --help ", h3_style)
    );
    println!(
        "  {}      Prints the version",
        ColoredString::new("-v, --version", h3_style)
    );
}

fn create_project_files(path: &Path) -> Result<()> {
    let project_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(
                ErrorKind::InvalidInput,
                "project path has no UTF-8 file name",
            )
        })?;
    fs::create_dir(path.join("src"))?;
    //fs::create_dir(path.join("test"))?;
    fs::write(path.join("src/main.nk"), "")?;
    //fs::write(path.join("test/main_test.nk"), "")?;
    let toml_template = format!(
        "[project]\nname = \"{}\"\nversion = \"0.1.0\"\n\n[dependencies]\n",
        project_name
    );
    fs::write(path.join("Phage.toml"), toml_template.as_bytes())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phage_toml_is_valid_toml() -> Result<()> {
        let base = env::temp_dir().join(format!(
            "phage-test-{}-{}",
            process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&base)?;
        let project = base.join("demo");
        fs::create_dir(&project)?;

        create_project_files(&project)?;
        let text = fs::read_to_string(project.join("Phage.toml"))?;
        fs::remove_dir_all(&base)?;

        assert_eq!(
            text,
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\n"
        );
        Ok(())
    }
}
