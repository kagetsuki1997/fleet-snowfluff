{
  pkgs,
}:

pkgs.buildNpmPackage {
  pname = "fleet-snowfluff-ui";
  version = "0.1.0";

  src = ../ui;

  npmDepsHash = "sha256-S0ET4rEuCxyock5qID4Lb2jmHb/i7vgp6v83R8xT7Vs=";

  npmBuildScript = "build";

  installPhase = ''
    mkdir -p $out
    cp -r dist $out/
  '';
}
