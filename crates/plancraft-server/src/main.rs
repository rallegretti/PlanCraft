//! `plancraft-server`: see `plancraft_server::cli`.

fn main() {
    std::process::exit(plancraft_server::cli::run(std::env::args_os().skip(1).collect()));
}
