#[cfg(feature = "enable-useless")]
use protobuf_codegen::Customize;

fn main() {
    println!("cargo:rerun-if-changed=src/config/geosite.proto");
    println!("cargo:rerun-if-changed=src/config/geoip.proto");
    println!("cargo:rerun-if-changed=src/api/api.proto");
    #[cfg(feature = "enable-useless")]
    tonic_build::configure()
        .build_client(false)
        .protoc_arg("--experimental_allow_proto3_optional")
        .compile_protos(&["src/api/api.proto"], &["src/api/"])
        .unwrap();
    #[cfg(feature = "enable-useless")]
    let customize = Customize::default()
        .gen_mod_rs(false)
        .tokio_bytes(true)
        .generate_getter(true);
    #[cfg(feature = "enable-useless")]
    protobuf_codegen::Codegen::new()
        .out_dir("src/")
        .customize(customize)
        .inputs(["src/config/geoip.proto", "src/config/geosite.proto"])
        .include(".")
        .out_dir("src/config/")
        .run()
        .expect("protoc");
}
