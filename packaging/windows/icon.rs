// Shared by the build scripts of casement and casement-host (included there): embeds
// data/casement.ico so Explorer, the taskbar and the installer show Casement's icon.
// Nothing happens for other targets.

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn embed_icon() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let icon = manifest.join("../../data/casement.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("out dir"));
    let rc = out.join("icon.rc");
    let icon_path = icon.display().to_string().replace('\\', "/");
    std::fs::write(&rc, format!("1 ICON \"{icon_path}\"\n")).expect("write icon.rc");
    let object = out.join("icon.o");
    let windres =
        std::env::var("WINDRES").unwrap_or_else(|_| "x86_64-w64-mingw32-windres".to_string());
    let status = Command::new(&windres)
        .args(["-O", "coff", "-i"])
        .arg(&rc)
        .arg("-o")
        .arg(&object)
        .status()
        .unwrap_or_else(|e| panic!("{windres}: {e}"));
    assert!(status.success(), "{windres} failed");
    println!("cargo:rustc-link-arg-bins={}", Path::new(&object).display());
}
