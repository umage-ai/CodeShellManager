#![forbid(unsafe_code)]

fn main() {
    let argv: Vec<String> = std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect();
    let code = csm_cli::run(&argv, &csm_cli::Env::from_process());
    std::process::exit(code);
}
