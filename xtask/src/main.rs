mod layers;
mod macos;
mod parity;
mod source_index;

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has repository parent");
    let result = run(&args, root);
    if let Err(error) = result {
        eprintln!("xtask: {error}");
        std::process::exit(1);
    }
    if args.first().map(String::as_str) == Some("parity") {
        println!("parity contract matches source");
    }
}

fn run(args: &[String], root: &std::path::Path) -> Result<(), String> {
    match args {
        [command] if command == "parity-sources" => source_index::print(root),
        [command] if command == "layers" => layers::run(root),
        [command] if command == "parity" => parity::check(root),
        [command, path, arch] if command == "macos-deployment" =>
            macos::check(std::path::Path::new(path), arch, "12.0"),
        [command, path, arch] if command == "macos-deployment-files" =>
            macos::files(std::path::Path::new(path), arch),
        _ => Err("usage: cargo xtask <parity|parity-sources|layers> or macos-deployment <Mach-O> <arm64|x86_64>".into()),
    }
}
