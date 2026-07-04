fn main() {
    if let Err(error) = cielc::lsp::run_stdio() {
        eprintln!("ciel-lsp: {error}");
        std::process::exit(1);
    }
}
