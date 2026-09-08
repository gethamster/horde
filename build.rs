fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=HORDE_RELEASE_PUBLIC_KEY");
    let mut prost = tonic_prost_build::Config::new();
    prost.protoc_executable(protoc_bin_vendored::protoc_bin_path()?);
    tonic_prost_build::configure().compile_with_config(
        prost,
        &["proto/federation.proto"],
        &["proto"],
    )?;
    println!("cargo:rerun-if-changed=proto/federation.proto");
    Ok(())
}
