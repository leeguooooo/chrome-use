//! Build-script helper for the optional Windows metadata prototype.
//! Call after protocol generation/dashboard preparation, never before them.
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn failure(message: &str) -> Box<dyn std::error::Error> {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message).into()
}
fn record(hash: &mut Sha256, label: &[u8], bytes: &[u8]) {
    hash.update((label.len() as u64).to_le_bytes());
    hash.update(label);
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}
fn watched_file(hash: &mut Sha256, label: &str, path: &Path) -> Result<()> {
    println!("cargo:rerun-if-changed={}", path.display());
    let bytes = fs::read(path)?;
    record(hash, label.as_bytes(), &bytes);
    Ok(())
}
fn tree(hash: &mut Sha256, label: &str, root: &Path) -> Result<()> {
    println!("cargo:rerun-if-changed={}", root.display());
    let mut entries = fs::read_dir(root)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| failure("non-UTF8 build input path"))?;
        if [".git", "target"].contains(&name.as_str()) {
            continue;
        }
        let path = entry.path();
        let kind = entry.file_type()?;
        let child = format!("{label}/{name}");
        if kind.is_symlink() {
            return Err(failure(
                "symlinked generation inputs require explicit manifest support",
            ));
        }
        if kind.is_dir() {
            tree(hash, &child, &path)?;
        } else if kind.is_file() {
            watched_file(hash, &child, &path)?;
        } else {
            return Err(failure("non-regular generation input"));
        }
    }
    Ok(())
}
fn tool(hash: &mut Sha256, label: &str, executable: &str, arguments: &[&str]) -> Result<String> {
    let output = Command::new(executable).args(arguments).output()?;
    // The actual Microsoft 14.51 linker emits its version/help with status
    // 1100 for /?. Accept that observed help contract only, never other errors.
    let msvc_help = label == "linker-version"
        && arguments == ["/?"]
        && output.status.code() == Some(1100)
        && output
            .stdout
            .starts_with(b"Microsoft (R) Incremental Linker Version ");
    if (!output.status.success() && !msvc_help)
        || output.stdout.len() + output.stderr.len() > 128 * 1024
    {
        return Err(failure(&format!(
            "{label} version query failed: status={}, stdoutBytes={}, stderrBytes={}, stdoutPrefix={:?}, stderrPrefix={:?}",
            output.status,
            output.stdout.len(),
            output.stderr.len(),
            String::from_utf8_lossy(&output.stdout[..output.stdout.len().min(1024)]),
            String::from_utf8_lossy(&output.stderr[..output.stderr.len().min(1024)]),
        )));
    }
    record(hash, label.as_bytes(), &output.stdout);
    record(hash, format!("{label}/stderr").as_bytes(), &output.stderr);
    record(
        hash,
        format!("{label}/status").as_bytes(),
        output.status.to_string().as_bytes(),
    );
    Ok(format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}
fn generation() -> Result<[u8; 16]> {
    let text = env::var("CHROME_USE_BUILD_GENERATION")
        .map_err(|_| failure("prototype Windows builds require a fresh 32-hex CHROME_USE_BUILD_GENERATION per Cargo invocation"))?;
    if text.len() != 32 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(failure("generation must be exactly 32 hex characters"));
    }
    let mut result = [0u8; 16];
    for (i, byte) in result.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16)?;
    }
    if result.iter().all(|b| *b == 0) {
        return Err(failure("zero generation is reserved"));
    }
    Ok(result)
}

pub fn emit() -> Result<()> {
    for name in [
        "CHROME_USE_BUILD_GENERATION",
        "CHROME_USE_MARKER_DEPENDENCIES",
        "RUSTC",
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    if env::var_os("CARGO_FEATURE_WINDOWS_BUILD_IDENTITY_PROTOTYPE").is_none()
        || env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
    {
        return Ok(());
    }
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("x86_64") {
        return Err(failure("prototype supports Windows x64 only"));
    }
    let generation = generation()?;
    let cli = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let root = cli
        .parent()
        .ok_or_else(|| failure("missing repository parent"))?;
    let out = PathBuf::from(env::var("OUT_DIR")?);
    let mut hash = Sha256::new();
    record(&mut hash, b"schema", b"chrome-use-cufg-input-manifest-v1");
    for input in ["Cargo.toml", "Cargo.lock", "build.rs", "build_marker.rs"] {
        watched_file(&mut hash, &format!("cli/{input}"), &cli.join(input))?;
    }
    // Source bytes include dirty/new files and in-source JS/PS1/JSON resources;
    // cli/tests, scripts and .github controller/workflow files are not product inputs.
    tree(&mut hash, "cli/src", &cli.join("src"))?;
    tree(&mut hash, "cli/cdp-protocol", &cli.join("cdp-protocol"))?;
    for input in ["skills", "skill-data", "packages/dashboard/out"] {
        tree(&mut hash, input, &root.join(input))?;
    }
    for input in [
        "extensions/ab-connect/manifest.json",
        "extensions/ab-connect/teach-recorder.js",
    ] {
        watched_file(&mut hash, input, &root.join(input))?;
    }
    watched_file(
        &mut hash,
        "generated/cdp_generated.rs",
        &out.join("cdp_generated.rs"),
    )?;
    for input in [".cargo/config", ".cargo/config.toml"] {
        let file = root.join(input);
        println!("cargo:rerun-if-changed={}", file.display());
        if file.exists() {
            watched_file(&mut hash, input, &file)?;
        }
    }
    // Prepared before Cargo build, offline/locked, for the exact target/features.
    // Reusing only Cargo.lock cannot catch edited path/vendor/registry sources.
    let metadata_path =
        PathBuf::from(env::var("CHROME_USE_MARKER_DEPENDENCIES").map_err(|_| {
            failure("provide locked cargo metadata JSON via CHROME_USE_MARKER_DEPENDENCIES")
        })?);
    println!("cargo:rerun-if-changed={}", metadata_path.display());
    let metadata: serde_json::Value = serde_json::from_slice(&fs::read(&metadata_path)?)?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| failure("metadata packages missing"))?;
    let mut dependencies = BTreeMap::new();
    let our_root = cli.canonicalize()?;
    for package in packages {
        let id = package["id"]
            .as_str()
            .ok_or_else(|| failure("metadata package ID missing"))?;
        let manifest = PathBuf::from(
            package["manifest_path"]
                .as_str()
                .ok_or_else(|| failure("metadata manifest path missing"))?,
        );
        let directory = manifest
            .parent()
            .ok_or_else(|| failure("dependency parent missing"))?
            .canonicalize()?;
        if directory == our_root {
            continue;
        }
        dependencies.insert(id.to_string(), directory);
    }
    if dependencies.is_empty() {
        return Err(failure("metadata dependency list is unexpectedly empty"));
    }
    for (id, directory) in dependencies {
        tree(&mut hash, &format!("dependency/{id}"), &directory)?;
    }
    // Include the exact resolved graph/features, including Cargo package path IDs.
    let resolve = metadata
        .get("resolve")
        .ok_or_else(|| failure("metadata resolution missing"))?;
    record(&mut hash, b"resolved-graph", &serde_json::to_vec(resolve)?);
    let mut variables: BTreeMap<String, String> = env::vars()
        .filter(|(key, _)| {
            key.starts_with("CARGO_CFG_")
                || key.starts_with("CARGO_FEATURE_")
                || key.starts_with("CARGO_PROFILE_")
                || key.starts_with("CARGO_TARGET_")
                || [
                    "TARGET",
                    "HOST",
                    "PROFILE",
                    "OPT_LEVEL",
                    "DEBUG",
                    "RUSTFLAGS",
                    "CARGO_ENCODED_RUSTFLAGS",
                    "RUSTC_WRAPPER",
                    "RUSTC_WORKSPACE_WRAPPER",
                ]
                .contains(&key.as_str())
        })
        .collect();
    let target = env::var("TARGET")?;
    let linker_key = format!(
        "CARGO_TARGET_{}_LINKER",
        target.to_ascii_uppercase().replace('-', "_")
    );
    println!("cargo:rerun-if-env-changed={linker_key}");
    let linker = env::var(&linker_key)
        .or_else(|_| env::var("RUSTC_LINKER"))
        .map_err(|_| {
            failure("set Cargo's actual target linker explicitly for the metadata prototype")
        })?;
    variables.insert("selected-linker".to_string(), linker.clone());
    for (name, value) in variables {
        println!("cargo:rerun-if-env-changed={name}");
        record(&mut hash, name.as_bytes(), value.as_bytes());
    }
    let compiler_version = tool(&mut hash, "rustc-vV", &env::var("RUSTC")?, &["-vV"])?;
    let linker_version = tool(
        &mut hash,
        "linker-version",
        &linker,
        if target.ends_with("-msvc") {
            &["/?"]
        } else {
            &["--version"]
        },
    )?;
    let digest: [u8; 32] = hash.finalize().into();
    let mut bytes = [0u8; 64];
    bytes[..8].copy_from_slice(b"CUFGPE01");
    for (offset, value) in [(8, 1u16), (10, 64), (12, 0x8664), (14, 1)] {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }
    bytes[16..48].copy_from_slice(&digest);
    bytes[48..].copy_from_slice(&generation);
    let text = format!("{:?}\n", bytes);
    fs::write(out.join("cufg_marker.rs"), text)?;
    fs::write(
        out.join("cufg_marker_manifest.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema":1,"target":target,"profile":env::var("PROFILE")?,"optLevel":env::var("OPT_LEVEL")?,
            "compiler":compiler_version,"linker":linker,"linkerVersion":linker_version,"marker":bytes.to_vec(),"scope":"matching artifact and observed embedded generation; not mapped-code integrity",
            "generationContract":"fresh external nonce per Cargo invocation; no relink under a reused nonce",
        }))?,
    )?;
    Ok(())
}
