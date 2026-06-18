//! Compiles the `kageha` connector API shim (`src/api_cli.rs`, std only) into a standalone
//! executable for the build target and leaves it in `OUT_DIR/kageha-api`. The library
//! embeds those bytes (`shell::cli_shim`) and installs them into the Toolchain_Cache when a
//! shell session starts, so the Host never has to locate a separate shipped file.
//!
//! Set `KAGEHA_SKIP_API_SHIM=1` to build without the shim (for example when cross-compiling
//! with a toolchain this nested `rustc` call can't link for). Sessions then start without a
//! `kageha` command and log why.
use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/api_cli.rs");
    println!("cargo:rerun-if-env-changed=KAGEHA_SKIP_API_SHIM");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let shim = out.join("kageha-api");
    if env::var_os("KAGEHA_SKIP_API_SHIM").is_some_and(|v| !v.is_empty() && v != "0") {
        fs::write(&shim, b"").expect("write empty shim");
        return;
    }
    let source = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR")).join("src/api_cli.rs");
    let entry = out.join("kageha_api_main.rs");
    fs::write(
        &entry,
        format!(
            "#[path = {:?}]\n#[allow(dead_code)]\nmod api_cli;\nfn main() {{\n    std::process::exit(api_cli::main())\n}}\n",
            source.to_str().expect("UTF-8 manifest path")
        ),
    )
    .expect("write shim entry point");
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let target = env::var("TARGET").expect("TARGET");
    let mut command = Command::new(rustc);
    command
        .args(["--edition", "2021", "--crate-type", "bin", "--crate-name", "kageha_api"])
        .args(["--target", &target, "--cap-lints", "allow"])
        .args(["-C", "opt-level=s", "-C", "debuginfo=0", "-C", "codegen-units=1", "-C", "panic=abort"])
        .arg("-o")
        .arg(&shim)
        .arg(&entry);
    if let Some(linker) = env::var_os("RUSTC_LINKER") {
        let mut flag = std::ffi::OsString::from("linker=");
        flag.push(linker);
        command.arg("-C").arg(flag);
    }
    let status = command.status().unwrap_or_else(|error| panic!("could not run rustc for the kageha CLI shim: {error}"));
    if !status.success() {
        panic!("building the kageha CLI shim failed ({status}); set KAGEHA_SKIP_API_SHIM=1 to build without it");
    }
}
