//! BedrockOS build driver. Runs on the host (Windows, Linux or macOS), never inside the OS.
//!
//! ```text
//! cargo xtask build      release kernel -> out/bedrockos.efi, out/usb/, out/bedrockos.img, out/bedrockos.vdi
//! cargo xtask test       host unit tests of the library
//! cargo xtask clippy     clippy -D warnings for the UEFI build and the host library
//! cargo xtask all        clippy + test + build
//! ```
//! `out/bedrockos.vdi` is only produced when VirtualBox's `VBoxManage` is found.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask lives in a subfolder").to_path_buf()
}

fn host_target() -> &'static str {
    if cfg!(windows) {
        "x86_64-pc-windows-gnu"
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") { "aarch64-apple-darwin" } else { "x86_64-apple-darwin" }
    } else {
        "x86_64-unknown-linux-gnu"
    }
}

fn run(cmd: &mut Command) -> Result<(), String> {
    eprintln!("+ {:?}", cmd);
    let st = cmd.status().map_err(|e| format!("cannot start {:?}: {e}", cmd.get_program()))?;
    if st.success() { Ok(()) } else { Err(format!("{:?} failed with {st}", cmd.get_program())) }
}

fn cargo() -> Command {
    let mut c = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.current_dir(root());
    c
}

fn python() -> Result<String, String> {
    for p in ["python3", "python", "py"] {
        if Command::new(p).arg("--version").output().map(|o| o.status.success()).unwrap_or(false) {
            return Ok(p.to_string());
        }
    }
    Err("Python 3 not found (needed to build the disk image)".into())
}

/// Finds VBoxManage on PATH or in the usual install folders.
pub fn vboxmanage() -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = vec![PathBuf::from("VBoxManage")];
    if let Ok(p) = env::var("VBOX_MSI_INSTALL_PATH") {
        cands.push(Path::new(&p).join("VBoxManage.exe"));
    }
    cands.push(PathBuf::from(r"C:\Program Files\Oracle\VirtualBox\VBoxManage.exe"));
    cands.push(PathBuf::from("/usr/bin/VBoxManage"));
    cands.push(PathBuf::from("/usr/local/bin/VBoxManage"));
    cands.push(PathBuf::from("/Applications/VirtualBox.app/Contents/MacOS/VBoxManage"));
    cands.into_iter().find(|c| Command::new(c).arg("--version").output().map(|o| o.status.success()).unwrap_or(false))
}

fn sha256_hex(path: &Path) -> Result<String, String> {
    // Small dependency-free SHA-256 so the build needs nothing beyond Rust and Python.
    let data = fs::read(path).map_err(|e| e.to_string())?;
    Ok(sha256::hex(&data))
}

fn build() -> Result<(), String> {
    let r = root();
    run(cargo().args(["build", "--release", "--package", "bedrockos"]))?;
    let efi = r.join("target/x86_64-unknown-uefi/release/bedrockos.efi");
    let out = r.join("out");
    let usb = out.join("usb");
    let _ = fs::remove_dir_all(&usb);
    fs::create_dir_all(usb.join("EFI/BOOT")).map_err(|e| e.to_string())?;
    fs::copy(&efi, out.join("bedrockos.efi")).map_err(|e| e.to_string())?;
    fs::copy(&efi, usb.join("EFI/BOOT/BOOTX64.EFI")).map_err(|e| e.to_string())?;
    fs::copy(r.join("config/bedrock.cfg"), usb.join("EFI/BOOT/bedrock.cfg")).map_err(|e| e.to_string())?;
    let fw = r.join("firmware");
    if fw.is_dir() {
        let dst = usb.join("firmware");
        fs::create_dir_all(&dst).map_err(|e| e.to_string())?;
        for e in fs::read_dir(&fw).map_err(|e| e.to_string())?.flatten() {
            if e.path().is_file() {
                fs::copy(e.path(), dst.join(e.file_name())).map_err(|e| e.to_string())?;
            }
        }
    }
    let sum = sha256_hex(&efi)?;
    fs::write(usb.join("SHA256.txt"), format!("{sum}  EFI/BOOT/BOOTX64.EFI\n")).map_err(|e| e.to_string())?;
    let size = fs::metadata(&efi).map_err(|e| e.to_string())?.len();
    eprintln!("bedrockos.efi: {size} bytes, sha256 {sum}");

    let img = out.join("bedrockos.img");
    run(Command::new(python()?).arg(r.join("tools/mkimage.py")).arg(&usb).arg(&img))?;

    match vboxmanage() {
        Some(vbm) => {
            let vdi = out.join("bedrockos.vdi");
            if vdi.exists() {
                // A VM may still have the old disk registered; the VM scripts re-attach the new one.
                let _ = Command::new(&vbm).args(["closemedium", "disk"]).arg(&vdi).output();
                fs::remove_file(&vdi).map_err(|e| format!("cannot replace {}: {e} (is the VM running?)", vdi.display()))?;
            }
            run(Command::new(&vbm).args(["convertfromraw"]).arg(&img).arg(&vdi).args(["--format", "VDI"]))?;
        }
        None => eprintln!("VBoxManage not found: skipped out/bedrockos.vdi (the .img and usb/ folder are ready)"),
    }
    eprintln!("\nready:\n  out/bedrockos.efi\n  out/usb/        (copy to a FAT32 USB stick)\n  out/bedrockos.img\n  out/bedrockos.vdi (if VirtualBox is installed)");
    Ok(())
}

fn test() -> Result<(), String> {
    run(cargo().args(["test", "--package", "bedrockos", "--lib", "--target", host_target()]))
}

fn clippy() -> Result<(), String> {
    run(cargo().args(["clippy", "--release", "--package", "bedrockos", "--", "-D", "warnings"]))?;
    run(cargo().args(["clippy", "--package", "bedrockos", "--lib", "--target", host_target(), "--", "-D", "warnings"]))
}

fn main() -> ExitCode {
    let cmd = env::args().nth(1).unwrap_or_else(|| "help".into());
    let res = match cmd.as_str() {
        "build" => build(),
        "test" => test(),
        "clippy" => clippy(),
        "all" => clippy().and_then(|_| test()).and_then(|_| build()),
        _ => {
            eprintln!("usage: cargo xtask <build|test|clippy|all>");
            return ExitCode::from(2);
        }
    };
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

mod sha256 {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
        0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
        0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
        0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    pub fn hex(data: &[u8]) -> String {
        let mut h: [u32; 8] =
            [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
        let mut msg = data.to_vec();
        let bits = (data.len() as u64).wrapping_mul(8);
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bits.to_be_bytes());
        for chunk in msg.chunks(64) {
            let mut w = [0u32; 64];
            for i in 0..16 {
                w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
            }
            let mut v = h;
            for i in 0..64 {
                let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
                let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
                let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
                let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
                let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
                let t2 = s0.wrapping_add(maj);
                v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
            }
            for i in 0..8 {
                h[i] = h[i].wrapping_add(v[i]);
            }
        }
        h.iter().map(|x| format!("{x:08x}")).collect()
    }

    #[test]
    fn abc() {
        assert_eq!(hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
