fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_path = if std::path::Path::new("../proto/quilt.proto").exists() {
        // Building from quilt-cli directory (nix build)
        "../proto/quilt.proto"
    } else if std::path::Path::new("proto/quilt.proto").exists() {
        // Building from workspace root (cargo build)
        "proto/quilt.proto"
    } else {
        panic!("Could not find quilt.proto file");
    };

    let include_path = if std::path::Path::new("../proto/").exists() {
        "../proto/"
    } else {
        "proto/"
    };

    tonic_build::configure()
        .build_server(false) // We only need the client for quilt-cli
        .compile(
            &[proto_path], 
            &[include_path]
        )?;
    Ok(())
} 