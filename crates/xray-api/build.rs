use std::error::Error;

const PROTOS: [&str; 3] = [
    "app/router/command/command.proto",
    "app/observatory/command/command.proto",
    "app/stats/command/command.proto",
];

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=proto");

    // Без protoc: protobuf разбирает protox, а prost получает готовые дескрипторы.
    // Комментарии из .proto в код не попадают: отступы в них prost принимает за
    // блоки кода, и rustdoc пытается запустить их как тесты.
    let mut compiler = protox::Compiler::new(["proto"])?;
    compiler.include_imports(true);
    compiler.include_source_info(false);
    compiler.open_files(PROTOS)?;

    tonic_prost_build::configure()
        .build_server(false)
        .build_transport(false)
        .include_file("mod.rs")
        .compile_fds(compiler.file_descriptor_set())?;
    Ok(())
}
