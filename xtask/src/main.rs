fn main() {
    let task = std::env::args().nth(1).unwrap_or_default();
    eprintln!("unknown task {task:?}\nusage: cargo xtask <task>");
    std::process::exit(1);
}
