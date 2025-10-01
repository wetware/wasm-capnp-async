fn main() {
    // Re-run build script if the schema changes
    // Use absolute, canonicalized paths so src_prefix matches the file path and
    // the generated module name is just `echo_capnp`.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let schema_dir = std::path::Path::new(&manifest_dir)
        .join("../lib/cap")
        .canonicalize()
        .expect("failed to canonicalize schema dir");

    println!(
        "cargo:rerun-if-changed={}",
        schema_dir.join("echo.capnp").display()
    );

    capnpc::CompilerCommand::new()
        .src_prefix(&schema_dir)
        .file(schema_dir.join("echo.capnp"))
        .run()
        .expect("schema compiler command");
}
