# One factory → two modules. `mode` is "home" or "nixos".
{ flake, mode }:
{ config, lib, pkgs, ... }:

let
  cfg = config.programs.chrome-use;
  defaultPkg = flake.packages.${pkgs.system}.default;

  storeExtId = "knfcmbamhjmaonkfnjhldjedeobeafmk";
  updateUrl = "https://clients2.google.com/service/update2/crx";
  policy = { ExtensionInstallForcelist = [ "${cfg.extensionId};${updateUrl}" ]; };

  homeOpts = {
    runOnActivation = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Run `chrome-use extension connect` on activation to register the user-level native-messaging host.";
    };
    connectFlags = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ "--keep-banner" ];
      description = "Flags for `chrome-use extension connect` (default skips restarting a running Chrome).";
    };
  };

  nixosOpts = {
    forceInstallExtension = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Force-install the chrome-use extension via Chrome managed policy (system-level; NixOS only).";
    };
    extensionId = lib.mkOption {
      type = lib.types.str;
      default = storeExtId;
      description = "Chrome Web Store id of the extension to force-install.";
    };
  };
in
{
  options.programs.chrome-use = {
    enable = lib.mkEnableOption "chrome-use (drive your real Chrome from an AI agent)";
    package = lib.mkOption {
      type = lib.types.package;
      default = defaultPkg;
      defaultText = lib.literalExpression "chrome-use flake package for this system";
      description = "The chrome-use package to install.";
    };
  } // lib.optionalAttrs (mode == "home") homeOpts
    // lib.optionalAttrs (mode == "nixos") nixosOpts;

  config = lib.mkIf cfg.enable (
    if mode == "home" then {
      home.packages = [ cfg.package ];
      home.activation = lib.optionalAttrs cfg.runOnActivation {
        chromeUseConnect = lib.hm.dag.entryAfter [ "writeBoundary" ] ''
          run ${lib.getExe cfg.package} extension connect ${lib.escapeShellArgs cfg.connectFlags} || true
        '';
      };
    } else {
      environment.systemPackages = [ cfg.package ];
      environment.etc = lib.mkIf cfg.forceInstallExtension {
        "opt/chrome/policies/managed/chrome-use.json".text = builtins.toJSON policy;
        "chromium/policies/managed/chrome-use.json".text = builtins.toJSON policy;
      };
    }
  );
}
