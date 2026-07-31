fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure().compile_protos(
        &["whisper-grpc/proto/transcribe.proto"],
        &["whisper-grpc/proto"],
    )?;
    Ok(())
}
