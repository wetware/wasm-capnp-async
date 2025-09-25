fn main() {
    // Re-run build script if the schema changes
    println!("cargo:rerun-if-changed=echo.capnp");

    capnpc::CompilerCommand::new()
        .file("echo.capnp")
        .run()
        .expect("schema compiler command");
    }
