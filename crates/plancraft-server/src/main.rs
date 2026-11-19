//! `plancraft-server`: see `plancraft_server::cli`.

fn main() {
    let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    std::process::exit(plancraft_server::cli::run(args));
}
