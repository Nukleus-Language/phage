use std::env;
use std::fs;
use std::process;
use std::path::Path;
use std::io::Result;

// existing Phage and Package structs and impls omitted for brevity...

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
            create_project_files(&path).unwrap();
        }
        "install" => {
            // previous install code omitted for brevity...
        }
        // other commands omitted for brevity...
        _ => {
            println!("Invalid command.");
            process::exit(1);
        }
    }
}
fn print_help_main() {
    println!("Phage, a package and build manager for Nukleus");
    println!("\nCommands:");
    println!("  init               Create a new project in the current directory");
    println!("  new                Create a new project in a new directory");
    println!("  inject             Inject a gene");
    println!("  remove             Remove a gene");
    println!("  build              Build the current project");
    println!("  run                Run the current project");
    println!("  list               List installed genes");
    println!("  -h, --help         Print this message");
    println!("  -v, --version      Prints the version");
}

fn create_project_files(path: &Path) -> Result<()> {
    fs::create_dir(path.join("src"))?;
    fs::create_dir(path.join("test"))?;
    fs::write(path.join("src/main.nk"), "")?;
    fs::write(path.join("test/main_test.nk"), "")?;
    fs::write(path.join("package.toml"), "{\"name\": \"\",\"version\": \"\",\"dependencies\": {}}")?;

    Ok(())
}

