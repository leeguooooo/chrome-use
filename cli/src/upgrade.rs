use crate::color;
use std::process::{exit, Command};

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Canonical installer for the stealth fork. `upgrade` just re-runs it, so the
/// upgrade path and the install path are identical (GitHub Release, no npm).
const INSTALL_URL: &str = "https://raw.githubusercontent.com/leeguooooo/chrome-use/main/install.sh";

/// Upgrade to the latest GitHub Release.
///
/// The stealth fork ships as a prebuilt binary attached to a GitHub Release —
/// NOT via the npm registry. Earlier this command (inherited from upstream)
/// ran `npm/pnpm install -g chrome-use@latest`, which installed the
/// UNRELATED upstream `chrome-use` package and clobbered the user's setup.
/// Now `upgrade` simply re-runs install.sh into the same directory as the
/// current binary, so it always tracks the freshest GitHub Release.
pub fn run_upgrade() {
    println!(
        "{}",
        color::cyan(&format!(
            "Upgrading chrome-use (currently v{}) from the latest GitHub Release...",
            CURRENT_VERSION
        ))
    );

    #[cfg(windows)]
    {
        eprintln!(
            "{} Automatic upgrade isn't supported on Windows.",
            color::warning_indicator()
        );
        eprintln!("  Download the latest chrome-use-win32-x64.tar.gz from:");
        eprintln!("    https://github.com/leeguooooo/chrome-use/releases/latest");
        eprintln!("  and replace chrome-use.exe on your PATH.");
        exit(1);
    }

    #[cfg(not(windows))]
    {
        // Install into the SAME directory as the running binary (in-place
        // upgrade), so we don't create a second copy elsewhere on PATH.
        let bin_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.canonicalize().ok())
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));

        let install_cmd = format!("curl -fsSL {} | sh", INSTALL_URL);
        println!("Running: {}", install_cmd);

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&install_cmd);
        if let Some(ref dir) = bin_dir {
            cmd.env("AGENT_BROWSER_BIN_DIR", dir);
        }

        let ok = cmd.status().map(|s| s.success()).unwrap_or(false);
        if ok {
            println!(
                "{} Upgrade complete — run `chrome-use --version` to confirm.",
                color::success_indicator()
            );
        } else {
            eprintln!(
                "{} Upgrade failed. Install manually:",
                color::error_indicator()
            );
            eprintln!("  curl -fsSL {} | sh", INSTALL_URL);
            exit(1);
        }
    }
}
