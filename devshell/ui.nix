{
  pkgs,
  version,
}:

pkgs.buildNpmPackage {
  pname = "fleet-snowfluff-ui";
  inherit version;

  src = ../ui;

  npmDepsHash = "sha256-S0ET4rEuCxyock5qID4Lb2jmHb/i7vgp6v83R8xT7Vs=";

  npmBuildScript = "build";

  installPhase = ''
    mkdir -p $out
    cp -r dist $out/
  '';
}
