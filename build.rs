extern crate bindgen;
extern crate avr_mcu;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

const AVR_ARCH: &'static str = "avr6";
const BINDINGS_DEST: &'static str = "src/bindings.rs";

/// Headers which can't be used from Rust.
const HEADER_BLACKLIST: &'static [&'static str] = &[
    "avr/crc16.h", "avr/parity.h", "avr/delay.h", // Deprecated, moved to 'util'
    "avr/signal.h", // Deprecated, moved to `avr/interrupt.h`
    "avr/wdt.h", // Requires MCU-specific constants
    "stdfix-avrlibc.h", // Deprecated, use 'stdfix.h' instead.
    "util/delay.h", "util/delay_basic.h", // relies on AVR-GCC specific optimisations
    "util/setbaud.h", // mostly made of preprocessor magic
];

const DEVICE_SPECIFIC_HEADERS: &'static [&'static str] = &[
    "avr/boot.h",
    "avr/sleep.h",
    "util/crc16.h",
];

pub struct MakeResult {
    pub static_lib_dir: PathBuf,
}

fn mcu_name() -> Option<String> {
    if cfg!(arch = "avr") {
        Some(avr_mcu::current::mcu_name().expect("could not get mcu name"))
    } else {
        None
    }
}

/// Assume that if we're trying to target x86, we're probably
/// generating docs.
fn is_building_documentation() -> bool {
    if cfg!(arch = "avr") {
        false
    } else {
        true
    }
}

fn main() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let libc_dir = manifest_dir.join("avr-libc");
    let include_dir = libc_dir.join("include");
    let arch_dir = libc_dir.join("avr").join("lib").join(AVR_ARCH);
    let static_lib_path = arch_dir.join("libc.a");

    if mcu_name().is_none() {
        println!("cargo:warning=not targeting a specific microcontroller, create a custom target specification to enable mcu-specific functionality");
    }

    if !static_lib_path.exists() && !is_building_documentation() {
        println!("avr-libc not yet built for '{}', building now", AVR_ARCH);
        bootstrap(&libc_dir);
        configure(&libc_dir);

        make(&include_dir);
        make(&arch_dir);
    }

    generate_bindings(&libc_dir);

    println!("cargo:rustc-link-search={}", arch_dir.display());
    println!("cargo:rustc-link-lib=static=c");
}

fn bootstrap(libc_dir: &Path) {
    println!("Bootstrapping avr-libc");

    let mut cmd = Command::new("sh");
    cmd.arg("bootstrap");

    cmd.current_dir(libc_dir);
    println!("{:?}", cmd);

    if !cmd.status().expect("failed to bootstrap avr-libc").success() {
        panic!("failed to bootstrap");
    }
}

fn configure(libc_dir: &Path) {
    println!("Configuring avr-libc");

    let host = env::var("HOST").unwrap();;

    let mut cmd = Command::new("sh");
    cmd.arg("configure");
    cmd.arg(&format!("--build={}", host));
    cmd.arg("--host=avr");

    cmd.env("CC", "avr-gcc");

    cmd.current_dir(libc_dir);
    println!("{:?}", cmd);

    if !cmd.status().expect("failed to configure avr-libc").success() {
        panic!("failed to configure");
    }
}

fn make(dir: &Path) -> MakeResult {
    println!("Making avr-libc");

    let mut cmd = Command::new("make");
    cmd.current_dir(&dir);
    println!("{:?}", cmd);

    if !cmd.status().expect("failed to compile avr-libc").success() {
        panic!("failed to make");
    }

    MakeResult {
        static_lib_dir: Path::new(dir).to_owned(),
    }
}

fn headers_inside(dir: &Path, libc_path: &Path) -> Vec<PathBuf> {
    let mut headers = Vec::new();

    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_file() {
            match path.extension().clone() {
                Some(ext) if ext == "h" => {
                    if !is_header_blacklisted(&path, libc_path) {
                        headers.push(path.clone());
                    }
                },
                _ => (),
            }
        }
    }

    headers
}

fn is_header_blacklisted(path: &Path, libc_path: &Path) -> bool {
    if let Some(stem) = path.file_stem() {
        if stem.to_str().unwrap().starts_with("io") {
            return true;
        }
    }

    is_header_in_list(path, libc_path, HEADER_BLACKLIST) ||
        (mcu_name().is_none() && is_header_device_specific(path, libc_path))
}

fn is_header_device_specific(path: &Path, libc_path: &Path) -> bool {
    is_header_in_list(path, libc_path, DEVICE_SPECIFIC_HEADERS)
}

fn is_header_in_list(path: &Path, libc_path: &Path, list: &[&str]) -> bool {
    let include_path = libc_path.join("include");

    list.iter()
        .any(|header| include_path.join(header) == path)
}

fn base_headers(libc_dir: &Path) -> Vec<PathBuf> {
    let include_dir = libc_dir.join("include");
    let mut headers = Vec::new();

    headers.extend(headers_inside(&include_dir, libc_dir));
    headers.extend(headers_inside(&include_dir.join("util"), libc_dir));
    headers.extend(headers_inside(&include_dir.join("sys"), libc_dir));
    headers.extend(headers_inside(&include_dir.join("avr"), libc_dir));
    headers
}

fn mcu_define_name() -> Option<&'static str> {
    mcu_name().map(|name| match &name[..] {
        "atmega328" => "__AVR_ATmega328__",
        "atmega328p" => "__AVR_ATmega328P__",
        _ => panic!("unsupported mcu, please raise an avr-rust issue on GitHub to add a {} preprocessor name mapping", name),
    })
}

fn generate_bindings(libc_dir: &Path) {
    // Configure and generate bindings.
    let mut builder = bindgen::builder()
        .use_core()
        .ctypes_prefix("::rust_ctypes")
        .clang_arg("-Iavr-libc/include")
        .clang_arg("-ffreestanding");

    if let Some(define_name) = mcu_define_name() {
        builder = builder.clang_arg(format!("-D{}", define_name));
    }

    for header_path in base_headers(libc_dir) {
        builder = builder.header(header_path.display().to_string());
    }

    let bindings = builder
        .generate()
        .expect("failed to create bindings");

    // Write the generated bindings to an output file.
    bindings.write_to_file(BINDINGS_DEST)
        .expect("could not write bindings to file");
}

