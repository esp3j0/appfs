use std::process::Command;

fn main() {
    compile_appfs_grpc_bridge_proto();

    // Sandbox uses libunwind-ptrace which depends on liblzma and gcc_s.
    #[cfg(target_os = "linux")]
    {
        println!("cargo:rustc-link-lib=lzma");
        // libgcc_s provides _Unwind_RaiseException and other exception handling symbols
        println!("cargo:rustc-link-lib=dylib=gcc_s");
    }

    // Capture git version from tags for --version flag
    // Rerun if git HEAD changes (new commits or tags)
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs/tags");

    let version = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        })
        .map(|v| v.trim().to_string())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());

    println!("cargo:rustc-env=AGENTFS_VERSION={}", version);

    // Enable delay-load for winfsp on Windows
    // Note: This requires WinFsp to be installed from https://winfsp.dev/rel/
    // The winfsp crate includes pre-built bindings, no LLVM/bindgen needed
    #[cfg(target_os = "windows")]
    {
        winfsp::build::winfsp_link_delayload();
    }
}

fn compile_appfs_grpc_bridge_proto() {
    let proto_v1 = "../examples/appfs/grpc-bridge/proto/appfs_adapter_v1.proto";
    let connector_proto = "../examples/appfs/grpc-bridge/proto/appfs_connector.proto";
    let structure_proto = "../examples/appfs/grpc-bridge/proto/appfs_structure.proto";
    let include_dir = "../examples/appfs/grpc-bridge/proto";

    println!("cargo:rerun-if-changed={proto_v1}");
    println!("cargo:rerun-if-changed={connector_proto}");
    println!("cargo:rerun-if-changed={structure_proto}");

    if let Ok(protoc) = protoc_bin_vendored::protoc_bin_path() {
        std::env::set_var("PROTOC", protoc);
    }

    let v1_exists = std::path::Path::new(proto_v1).exists();
    let connector_exists = std::path::Path::new(connector_proto).exists();
    let structure_exists = std::path::Path::new(structure_proto).exists();
    if !v1_exists && !connector_exists && !structure_exists {
        return;
    }

    if v1_exists {
        tonic_build::configure()
            .build_server(true)
            .build_client(true)
            .compile_protos(&[proto_v1], &[include_dir])
            .expect("failed to compile AppFS gRPC bridge v1 proto");
    }
    if connector_exists {
        tonic_build::configure()
            .build_server(true)
            .build_client(true)
            .compile_protos(&[connector_proto], &[include_dir])
            .expect("failed to compile AppFS connector proto");
    }
    if structure_exists {
        tonic_build::configure()
            .build_server(true)
            .build_client(true)
            .compile_protos(&[structure_proto], &[include_dir])
            .expect("failed to compile AppFS structure proto");
    }
}
