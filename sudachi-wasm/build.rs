use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    // workspace root is parent of this crate's manifest dir
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir.parent().unwrap().to_path_buf();

    // Prepare paths
    let xcdat_dir = workspace_root.join("xcdat");
    let xcdat_build_dir = xcdat_dir.join("build-native");
    let xcdat_tools_dir = xcdat_build_dir.join("tools");
    let artifacts_dir = workspace_root.join("build-artifacts");
    let _ = std::fs::create_dir_all(&artifacts_dir);

    // 1) Build xcdat native tools (cmake)
    if xcdat_dir.exists() {
        println!("cargo:warning=Building xcdat native tools (cmake)");
        let _ = std::fs::create_dir_all(&xcdat_build_dir);

        // run cmake .. -DCMAKE_BUILD_TYPE=Release
        let status = Command::new("cmake")
            .current_dir(&xcdat_build_dir)
            .arg("..")
            .arg("-DCMAKE_BUILD_TYPE=Release")
            .status();

        match status {
            Ok(s) if s.success() => {}
            Ok(_) | Err(_) => {
                println!("cargo:warning=cmake failed; continuing and hoping prebuilt tools exist");
            }
        }

        // build
        let status = Command::new("cmake")
            .current_dir(&xcdat_build_dir)
            .arg("--build")
            .arg(".")
            .arg("--")
            .arg("-j")
            .status();

        match status {
            Ok(s) if s.success() => {}
            Ok(_) | Err(_) => {
                println!("cargo:warning=cmake --build failed; build may still succeed if tools are present");
            }
        }
    } else {
        println!("cargo:warning=xcdat directory not found; skipping native xcdat build");
    }

    // Paths to xcdat helper binaries (may not exist)
    let xcdat_build_bin = xcdat_tools_dir.join("xcdat_build");
    let xcdat_enumerate_bin = xcdat_tools_dir.join("xcdat_enumerate");

    // 2) Build sudachi-xcdat-compress binary using cargo
    println!("cargo:warning=Building sudachi-xcdat-compress");

    // prefer rustup run stable cargo if available
    let cargo_cmd = if which::which("rustup").is_ok() {
        let mut c = Command::new("rustup");
        c.arg("run").arg("stable").arg("cargo");
        c
    } else {
        Command::new("cargo")
    };

    // use workspace manifest path and build specific package/bin to avoid nested-cwd issues
    let manifest_path = workspace_root.join("Cargo.toml");
    let mut build_cmd = cargo_cmd;
    build_cmd
        .arg("build")
        .arg("--manifest-path")
        .arg(manifest_path)
        .arg("--package")
        .arg("sudachi-cli")
        .arg("--bin")
        .arg("sudachi-xcdat-compress")
        .arg("--release");

    let status = build_cmd.status();
    match status {
        Ok(s) if s.success() => {}
        Ok(_) | Err(_) => {
            println!("cargo:warning=failed to build sudachi-xcdat-compress; continuing");
        }
    }

    // binary path
    let compressor_bin = workspace_root
        .join("target")
        .join("release")
        .join("sudachi-xcdat-compress");
    if compressor_bin.exists() {
        println!("cargo:warning=Running sudachi-xcdat-compress to generate dictionary");
        let out_file = artifacts_dir.join("system_core.xdic");
        let resources_dic = workspace_root.join("resources").join("system.dic");

        let mut cmd = Command::new(&compressor_bin);
        cmd.arg(resources_dic).arg("-o").arg(&out_file);

        // pass env vars for xcdat tools if present
        if xcdat_build_bin.exists() {
            cmd.env("XCDAT_BUILD_BIN", xcdat_build_bin);
        }
        if xcdat_enumerate_bin.exists() {
            cmd.env("XCDAT_ENUMERATE_BIN", xcdat_enumerate_bin);
        }

        let status = cmd.status();
        match status {
            Ok(s) if s.success() => {
                println!("cargo:warning=Generated {}", out_file.display());
            }
            Ok(_) | Err(_) => {
                println!("cargo:warning=Failed to run compressor to generate dictionary");
            }
        }
    } else {
        println!(
            "cargo:warning=compressor binary not found at {}; skipping dictionary build",
            compressor_bin.display()
        );
    }

    // Invalidate build if resources change
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root
            .join("resources")
            .join("system.dic")
            .display()
    );
}
