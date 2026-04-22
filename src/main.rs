use std::env;
use std::process;

fn main() {
    let args: Vec<String> = env::args().collect();
    let cfg = quadcd::Config::from_env();
    let mut app = quadcd::App::new(cfg);
    let code = app.run(&args);
    process::exit(code);
}
