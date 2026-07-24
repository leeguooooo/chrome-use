{ lib, rustPlatform }:

let
  root = ../.;

  # Keep only what the crate + its build.rs / include_dir! actually need.
  # A file is included only if the filter returns true for it AND every
  # ancestor directory, so directory entries must be kept explicitly.
  src = lib.cleanSourceWith {
    src = root;
    filter = path: _type:
      let
        rel = lib.removePrefix (toString root + "/") (toString path);
        keepTree = p: rel == p || lib.hasPrefix (p + "/") rel;
      in
        (keepTree "cli" && !(lib.hasInfix "/target/" ("/" + rel + "/")))
        || keepTree "skills"
        || keepTree "skill-data"
        || rel == "extensions"
        || rel == "extensions/ab-connect"
        || rel == "extensions/ab-connect/manifest.json";
  };
in
rustPlatform.buildRustPackage {
  pname = "chrome-use";
  version = (lib.importTOML ../cli/Cargo.toml).package.version;
  inherit src;

  cargoLock.lockFile = ../cli/Cargo.lock;
  # Crate root lives in cli/, not at the repo root: cargoSetupHook validates
  # Cargo.lock against $sourceRoot/Cargo.lock unless told where to look.
  cargoRoot = "cli";
  buildAndTestSubdir = "cli";

  # Tests need a real Chrome + network; run them in CI, not the Nix sandbox.
  doCheck = false;

  meta = {
    description = "Drive your real, logged-in Chrome from any AI agent";
    homepage = "https://github.com/leeguooooo/chrome-use";
    license = lib.licenses.asl20;
    mainProgram = "chrome-use";
  };
}
