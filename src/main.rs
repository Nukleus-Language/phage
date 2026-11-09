mod cli;
mod manifest;
mod project;
#[cfg(test)]
mod testutil;

use std::env;

fn main() {
    cli::run(&env::args().collect::<Vec<_>>());
}
