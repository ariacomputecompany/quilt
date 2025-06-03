fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(false) // We only need the client for quilt-cli
        .compile(
            &["../proto/quilt.proto"], // Corrected path to the proto file
            &["../proto/"] // Corrected include path for any imports
        )?;
    Ok(())
} 