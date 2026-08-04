# Libretro submission

This repository is ready to be mirrored by Libretro and built with the shared Rust CI templates.

## Supported buildbot targets

- Windows x64
- Linux x64
- macOS x64
- macOS ARM64

The VDP contains native C++ code, so each target must build from a recursive clone with `make`, a C++17 compiler, and `ar` available. `.gitlab-ci.yml` sets `GIT_SUBMODULE_STRATEGY: recursive`.

## Core behavior

- Boots Agon Console8 MOS with no content.
- Bundles the MOS image and target-specific VDP module in `agon_libretro`.
- Uses `system/agon/mos_console8.bin` and the platform `vdp_console8` library when present, allowing firmware overrides.
- Mounts `system/agon` as the SD card by default.
- Mounts the selected content file's parent directory as the SD card.
- Sends Libretro keyboard input to the emulated PS/2 keyboard.
- Outputs RGB565 video at 640×480, 60 Hz and stereo audio at 48 kHz.
- Does not yet support save states, rewind, cheats, joypad mappings, or mouse input.

## Proposed `libretro-super` changes

Copy `agon_libretro.info` to `dist/info/agon_libretro.info`.

Add this core rule to `rules.d/core-rules.sh`:

```sh
include_core_agon() {
    register_module core "agon"
}
libretro_agon_name="Fab Agon"
libretro_agon_git_url="https://github.com/isaiahpettingill/agon-emulator-libretro.git"
libretro_agon_git_submodules="yes"
libretro_agon_build_makefile="Makefile.libretro"
```

Add `agon` only to the Windows x64, Linux x64, macOS x64, and macOS ARM64 buildbot recipe lists. Other targets have not been tested.

The source repository also has `.gitlab-ci.yml` entries for those four targets. Libretro must mirror the repository under `git.libretro.com` before those jobs can publish buildbot artifacts.

## Local checks

Native release build:

```sh
make -f Makefile.libretro
```

Confirm the expected Libretro exports:

```sh
nm -D agon_libretro.so | grep ' retro_'
```

On Windows, use `objdump -p agon_libretro.dll` instead. On macOS, use `nm -gU agon_libretro.dylib`.

Then install the core and `agon_libretro.info` in RetroArch and check:

1. Core Information reports the expected name, version, license, and no missing firmware.
2. Start Core boots MOS without content.
3. A directory containing known Agon programs is mounted as the SD card when one of its files is selected.
4. Keyboard input, video, and audio work.
5. Close Content and start the core again in the same RetroArch process.
