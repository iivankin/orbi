fn main() {
    if let Err(error) = orbit::run() {
        eprintln!("orbit: {error:#}");
        std::process::exit(1);
    }
}
