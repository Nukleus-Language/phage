mod build;
mod cli;
mod deps;
mod git;
mod lock;
mod manifest;
mod project;
#[cfg(test)]
mod testutil;

use std::env;

fn main() {
    cli::run(&env::args().collect::<Vec<_>>());
}
