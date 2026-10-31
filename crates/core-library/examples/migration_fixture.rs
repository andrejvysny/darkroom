#[path = "../tests/support/migration_catalog.rs"]
mod migration_catalog;

fn main() -> migration_catalog::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let output = args
        .next()
        .ok_or("usage: migration_fixture OUTPUT_DIRECTORY")?;
    if args.next().is_some() {
        return Err("usage: migration_fixture OUTPUT_DIRECTORY".into());
    }
    migration_catalog::generate(std::path::Path::new(&output))
}
