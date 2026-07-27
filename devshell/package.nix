{
  craneLib,
  pkgs,
  src,
  cargoArtifacts,
  name,
  version,
  ui,
}:

craneLib.cargoBuild {
  inherit src cargoArtifacts;

  pname = name;
  inherit version;

  nativeBuildInputs = with pkgs; [
    pkg-config
    wrapGAppsHook3
    cargo-tauri
    nodejs
    autoPatchelfHook
  ];

  buildInputs = with pkgs; [
    webkitgtk_4_1
    gtk3
    glib
    gdk-pixbuf
    cairo
    pango
    atk
    librsvg
    libayatana-appindicator
    alsa-lib

    mesa
    libglvnd
    # wgpu's default backend selection prefers Vulkan over GL when
    # Vulkan is actually discoverable; without the loader on
    # LD_LIBRARY_PATH (preFixup below), it silently falls back to the
    # GLES/EGL backend, which -- like the DX12/Vulkan-only situation
    # that forced abandoning wgpu on Windows entirely (see
    # platform/windows.rs's module doc) -- never advertises an
    # alpha-capable surface, so pet windows render fully opaque
    # regardless of the X11 ARGB visual or a running compositor.
    vulkan-loader

    gst_all_1.gstreamer
    gst_all_1.gst-plugins-base
    gst_all_1.gst-plugins-good
    gst_all_1.gst-plugins-bad
    gst_all_1.gst-libav
  ];

  buildPhase = ''
    cp crates/fleet-snowfluff/tauri.conf.json crates/fleet-snowfluff/tauri.conf.nix.json

    ${pkgs.jq}/bin/jq '
      .build.beforeBuildCommand = ""
    ' \
    crates/fleet-snowfluff/tauri.conf.nix.json \
    > crates/fleet-snowfluff/tauri.conf.json

      cp -r ${ui}/dist ui/dist

      cargo tauri build --no-bundle
  '';

  installPhase = ''
    mkdir -p $out/bin
    cp target/release/fleet-snowfluff $out/bin/
  '';

  postInstall = ''
    wrapGApp $out/bin/${name}
  '';

  preFixup = ''
    gappsWrapperArgs+=(
      --prefix LD_LIBRARY_PATH : ${
        pkgs.lib.makeLibraryPath [
          pkgs.libayatana-appindicator
          pkgs.vulkan-loader
        ]
      }
      # /run/opengl-driver/lib is where NixOS's own `hardware.graphics.
      # enable` (needed for basically any GPU acceleration -- almost
      # certainly already on for a GNOME/KDE desktop) publishes the
      # real GPU vendor's Vulkan ICD alongside its OpenGL driver.
      # vulkan-loader above only provides libvulkan.so.1 itself, not
      # an actual driver to load through it -- without this, the
      # loader can find no ICD and wgpu falls back to GL/EGL the same
      # way it would if vulkan-loader were entirely absent.
      --prefix LD_LIBRARY_PATH : /run/opengl-driver/lib
      # The settings window (the app's only webview) renders fully
      # blank rather than erroring -- a well-documented WebKitGTK-on-
      # NixOS symptom, separate from the wgpu issue above: WebKitGTK
      # tries to hardware-accelerate its own compositing via DMABUF/
      # GPU rendering, and when that negotiation fails silently (which
      # NixOS's non-standard driver paths make more likely than on an
      # FHS distro), it shows nothing instead of falling back to
      # software rendering. Disabling WebKit's own compositing mode is
      # the standard workaround; a settings form has no real need for
      # it anyway.
      --set WEBKIT_DISABLE_COMPOSITING_MODE 1
      --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : "${
        pkgs.lib.makeSearchPath "lib/gstreamer-1.0" [
          pkgs.gst_all_1.gst-plugins-base
          pkgs.gst_all_1.gst-plugins-good
          pkgs.gst_all_1.gst-plugins-bad
          pkgs.gst_all_1.gst-libav
        ]
      }"
    )
  '';
}
