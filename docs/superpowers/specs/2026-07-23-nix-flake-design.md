# Nix flake for chrome-use — design

**Date:** 2026-07-23
**Status:** Approved (design), pending implementation plan
**Scope:** Add a Nix flake providing a package, runnable app, dev shell, checks, and NixOS + home-manager modules.

## Goal

Make chrome-use consumable via Nix: `nix run`, `nix build`, `nix develop`, and
declarative install through a NixOS or home-manager module that also wires up the
Chrome native-messaging host and (where possible) the browser extension.

## Repo facts that shape the design

- **The product is the Rust binary** in `cli/` (`[[bin]] name = "chrome-use"`).
  `bin/chrome-use.js` + `scripts/postinstall.js` only download/dispatch prebuilt
  binaries for the npm distribution — irrelevant to Nix, which builds from source.
- **Pure Rust, no C/TLS system deps.** `boa_engine` (not `rquickjs`) was chosen
  specifically to avoid a C toolchain. Clean source build.
- **Single crate, no root workspace.** `Cargo.lock` lives at `cli/Cargo.lock`.
  Cargo is invoked as `--manifest-path cli/Cargo.toml`.
- **`build.rs` and `skills.rs` reach outside `cli/`:**
  - `skills.rs`: `include_dir!("$CARGO_MANIFEST_DIR/../skills")` and `../skill-data` —
    embedded at compile time.
  - `build.rs`: reads `../extensions/ab-connect/manifest.json` (embeds ext version),
    codegens CDP types from `cli/cdp-protocol/*.json` into `OUT_DIR`, and
    `create_dir_all("../packages/dashboard/out")` writing a placeholder `index.html`.
  - `native/stream/http.rs`: `rust-embed` folder `../packages/dashboard/out/`.
  - **Consequence:** the build source must be the **repo root** (filtered), not just
    `cli/`, and it must be writable (crane copies to a writable dir, so this works).
- **`packages/dashboard/` is not in the repo** (not git-tracked, absent on disk).
  `build.rs` writes the placeholder; `rust-embed` embeds it. The packaged binary
  therefore serves a "dashboard not built" placeholder. The live dashboard is out
  of scope — its source isn't here. No node/pnpm is needed to build the CLI.
- **Native-messaging host is self-installed by the binary.**
  `chrome-use extension connect` (and `doctor --fix`) writes an `nm-host.sh`
  launcher + host manifests into every Chromium-family `NativeMessagingHosts`
  directory under `$HOME`, under both host names (`com.leeguoo.chrome_use` and the
  legacy `com.agent_browser.connect`), with `allowed_origins` for both the unpacked
  extension id (deterministic, from the manifest `key`) and the Web Store id. It can
  also write a Chrome managed-policy profile to force-install the extension.
  **This logic is user-level (writes under `$HOME`).**

## Chosen stack

- **crane** for the Rust build. Reasons: needs a custom source filter (keep
  `skills/`, `skill-data/`, `extensions/ab-connect/manifest.json` alongside `cli/`),
  a `build.rs` that codegens + writes files, `include_dir!` + `rust-embed`, and we
  want cached `cargoArtifacts` plus clippy/test checks. Alternative considered:
  `rustPlatform.buildRustPackage` (no extra input but no dep caching);
  naersk (weaker custom source filtering). Crane wins on fit.
- **flake-parts** for structure. Clean multi-system × multi-output
  (package / app / devShell / checks + two modules). Alternative: plain flake with
  `nixpkgs.lib.genAttrs` — fewer inputs, more boilerplate.
- **nixpkgs stable Rust** (cargo/rustc/clippy/rustfmt). Edition 2021, no nightly
  features observed. Alternative: fenix/rust-overlay — add only if a pinned or
  nightly toolchain becomes necessary.

## File layout

```
flake.nix          # inputs + flake-parts wiring; perSystem outputs; flake-level modules
flake.lock         # committed lockfile
nix/package.nix    # crane build of cli/ → chrome-use binary
nix/module.nix     # shared option/config factory → nixosModules + homeManagerModules
```

Three hand-written files. `flake.nix` stays a thin assembler; the crane derivation
and the module logic live in `nix/`.

## Outputs

### `packages.default` (= `packages.chrome-use`)
Crane build of the `cli/` crate.
- `src`: repo root run through a crane source filter that keeps `cli/`, `skills/`,
  `skill-data/`, and `extensions/ab-connect/manifest.json` (plus the default
  Rust/Cargo file set). This satisfies every out-of-`cli/` path `build.rs`/`skills.rs`
  touch.
- Crate-in-subdir handling: `cargoLock = ./cli/Cargo.lock`; build cargo with
  `--manifest-path cli/Cargo.toml` (crane `cargoExtraArgs`), shared by the
  `cargoArtifacts` (deps-only) and final `buildPackage` steps.
- Version: inherited from `cli/Cargo.toml` (currently 1.5.77). `scripts/sync-version.js`
  is a JS-side concern; Nix reads the Cargo version directly and does not run it.
- `meta`: license Apache-2.0, mainProgram `chrome-use`, homepage/description from the
  crate manifest.

### `apps.default`
`type = "app"`, program → `${package}/bin/chrome-use`. Enables `nix run`.

### `devShells.default` (full)
`cargo rustc clippy rustfmt` + `nodejs_24` + `pnpm` (11.1.3) + `chromium` +
`vhs` + `jq` + `ripgrep`. Covers building/testing the Rust core and touching the JS
wrapper, extension, and docs/demo (`vhs assets/demo.tape`) without leaving the shell.
(Trimmable to rust-only later if the node side is never touched here.)

### `checks`
Crane-provided `cargo clippy` (deny warnings) and `cargo test` (nextest), wired into
`nix flake check`. Reuse the `cargoArtifacts` from the package build so checks don't
recompile dependencies.

### `nixosModules.default` and `homeManagerModules.default`
Generated from one shared factory in `nix/module.nix`. The two are **asymmetric by
necessity** because the binary's native-host registration is user-level while Chrome
force-install policy is system-level:

- **`homeManagerModules.default`** (the workhorse for native messaging):
  - Adds the package to the user profile.
  - On activation (home-manager activation script or a `systemd.user` oneshot),
    runs `chrome-use extension connect` so the user-level native-messaging host
    manifests + launcher are written for whatever Chromium-family browsers exist.
    This reuses the binary's own robust per-browser logic — no manifest generation
    duplicated in Nix (the strategy chosen during brainstorming).
  - Extension: installed by the user from the Chrome Web Store (the `connect` flow
    can surface the store page). Force-install is **not** available from a user-level
    module (policy dirs are root-owned `/etc`), and this is documented in the option.
  - Options: `enable`, `package`, `browsers` (which Chromium-family targets to
    register for; default: autodetect via the binary), `runOnActivation` (bool,
    default true).

- **`nixosModules.default`** (system-wide install + force-install policy):
  - Adds the package to `environment.systemPackages`.
  - When `forceInstallExtension = true` (NixOS-only option), writes
    `/etc/opt/chrome/policies/managed/chrome-use.json` (and the
    `/etc/chromium/policies/managed/` variant) containing an `ExtensionInstallForcelist`
    entry for the Web Store extension id, so the extension is force-installed
    declaratively.
  - Native-host registration remains per-user: the module documents that users run
    `chrome-use extension connect` once, or enable the home-manager module. (A
    system-level native-messaging host under `/etc/opt/chrome/native-messaging-hosts/`
    is a possible future addition but would reintroduce Nix-side manifest generation,
    which we deliberately avoided.)
  - Options: `enable`, `package`, `forceInstallExtension` (bool, default false),
    `extensionId` (Web Store id; default from the shipped extension).

### `formatter`
`nixpkgs-fmt` (or `alejandra`) so `nix fmt` works. One line; skip if undesired.

## Deliberate simplifications (ponytail)

- Packaged binary ships the **placeholder dashboard** — real dashboard source isn't
  in-repo. Add when it lands: build `packages/dashboard` and feed its `out/` into the
  crane `src`.
- Module native-host step is **imperative-on-activation** (runs the binary's own
  command) rather than Nix-generated manifests — zero logic duplication, tracks the
  tool automatically.
- Node wrapper / `postinstall.js` / prebuilt-binary download are **ignored**; Nix
  builds from source.
- No fenix/rust-overlay; nixpkgs stable Rust until a pinned toolchain is actually
  needed.

## Testing / verification

- `nix build .#default` produces a working `chrome-use` binary; `result/bin/chrome-use --version`
  reports 1.5.77 and `--help` runs.
- `nix run .#default -- doctor` executes (exercises the embedded skills/skill-data +
  placeholder dashboard path).
- `nix flake check` passes clippy + tests.
- `nix develop -c cargo build --manifest-path cli/Cargo.toml` succeeds in the shell.
- Module smoke test: evaluate `nixosConfigurations`/`homeConfigurations` in a
  `nix flake check` VM test or at minimum `nix eval` the module to confirm it builds
  the config (option wiring + policy file content).

## Open questions

None blocking. `browsers` autodetection behavior and whether to also emit a system
native-messaging host in the NixOS module are noted above as future work, not part of
this iteration.
