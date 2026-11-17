mod build;
mod cli;
mod deps;
mod error;
mod fingerprint;
mod git;
mod help;
mod lock;
mod manifest;
mod profile;
mod project;
#[cfg(test)]
mod testutil;
mod ui;
mod workspace;

use std::env;

fn main() {
    cli::run(&env::args().collect::<Vec<_>>());
}
