fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(false) // We only need the client for quilt-cli
        .compile(
            &["proto/quilt.proto"], // Local proto file in build dir
            &["proto/"] // Local proto directory in build dir  
        )?;
    Ok(())
} 