extern crate clap;
use clap::{Arg, App, SubCommand};

fn main() {

    let matches = App::new("organ")
                        .version("alpha 1")
                        .author("Skuld Norniern. <skuldnorniern@gmail.com>")
                        .about("Dependency manager for Nukleus ")
                        .subcommand(SubCommand::with_name("run")
                            .help("Runs the Nukleus Project")
                            .arg(Arg::with_name("path")
                                .value_name("Project Directory")
                                .takes_value(true)))
                        .subcommand(SubCommand::with_name("init")
                            .help("Initialize the Nukleus Project")
                            .arg(Arg::with_name("path")
                                .value_name("Project Directory")
                                .takes_value(true)))
                        .get_matches();



    if let Some(matches) = matches.subcommand_matches("run") {
        let run = matches.value_of("path").unwrap_or("main.nkl");
        println!("Running: {}", run);
    }

    if let Some(matches) = matches.subcommand_matches("init") {
        let pjpath = matches.value_of("path").unwrap_or("./");
        println!("Initialize Nukleus project in {}",pjpath);
    }
}