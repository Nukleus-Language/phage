use std::{
    env,
    fs,
    io::Result,
    path::Path,
    process,
};
use inksac::types::*;

// existing Phage and Package structs and impls omitted for brevity...
const ERRORSTYLE: Style = Style {
    forground: Some(Color::Red),
    background: Some(Color::Black),
    bold: false,
    dim: false,
    underline: false,
    italic: false,
};

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
            let path = env::current_dir().unwrap();
            create_project_files(&path).unwrap();
        }
        "new" => {
            if args.len() < 3 {
                println!("Please provide a project name.");
                process::exit(1);
            }

            fs::create_dir(&args[2]).unwrap();
            let path = Path::new(&args[2]);
            create_project_files(path).unwrap();
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
        forground: Some(Color::Cyan),
        ..Default::default()
    };
    let h2_style = Style {
        forground: Some(Color::Yellow),
        ..Default::default()
    };
    let h3_style = Style {
        forground: Some(Color::Green),
        ..Default::default()
    };
    let _test_style = Style {
        forground: Some(Color::RGB(102, 103, 171)),
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
    let project_name = path.file_name().unwrap().to_str().unwrap();
    fs::create_dir(path.join("src"))?;
    //fs::create_dir(path.join("test"))?;
    fs::write(path.join("src/main.nk"), "")?;
    //fs::write(path.join("test/main_test.nk"), "")?;
    let toml_template = format!("[project]\nname: \"{}\"\nversion:\"0.1.0\"\n\n[dependencies]", project_name);
    fs::write(
        path.join("Phage.toml"),
        toml_template.as_bytes()
    )?;

    Ok(())
}
