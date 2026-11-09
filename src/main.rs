mod cli;
mod manifest;
mod project;

use std::env;

fn main() {
    cli::run(&env::args().collect::<Vec<_>>());
}
