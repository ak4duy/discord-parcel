#!/usr/bin/env python3

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from datetime import datetime
from pathlib import Path

import tomllib
from windows_pe import imported_dlls

ROOT = Path(__file__).resolve().parents[1]
TARGET = "x86_64-pc-windows-gnu"
SVG_MODULE = Path("lib/gdk-pixbuf-2.0/2.10.0/loaders/pixbufloader_svg.dll")
SYSTEM_DLLS = frozenset(
    """
advapi32.dll bcrypt.dll bcryptprimitives.dll cfgmgr32.dll combase.dll
comctl32.dll comdlg32.dll crypt32.dll cryptbase.dll cryptsp.dll d2d1.dll
d3d11.dll d3d12.dll d3d9.dll dcomp.dll dbghelp.dll dnsapi.dll dwmapi.dll
dwrite.dll dxgi.dll gdi32.dll gdiplus.dll hid.dll imm32.dll iphlpapi.dll
kernel32.dll kernelbase.dll msimg32.dll msvcrt.dll ncrypt.dll netapi32.dll
normaliz.dll ntdll.dll ole32.dll oleaut32.dll opengl32.dll powrprof.dll
propsys.dll psapi.dll rpcrt4.dll secur32.dll setupapi.dll shcore.dll
shell32.dll shlwapi.dll ucrtbase.dll user32.dll userenv.dll usp10.dll
uxtheme.dll version.dll winhttp.dll wininet.dll winmm.dll winspool.drv
wintrust.dll wldap32.dll ws2_32.dll wtsapi32.dll
""".split()
)


def run(command, **kwargs):
    print("+", " ".join(map(str, command)), flush=True)
    return subprocess.run(list(map(str, command)), check=True, **kwargs)


def system_dll(name):
    name = name.lower()
    return name in SYSTEM_DLLS or name.startswith(("api-ms-win-", "ext-ms-win-"))


def dependency_closure(seeds, bin_dir):
    """Resolve both regular and delay-load imports, including explicit modules."""
    available = {}
    for path in sorted(bin_dir.iterdir()):
        if path.suffix.lower() == ".dll":
            key = path.name.lower()
            if key in available:
                raise ValueError(f"Duplicate case-insensitive DLL name: {path.name}")
            available[key] = path
    pending = list(seeds)
    seen = set()
    dependencies = {}
    while pending:
        path = pending.pop()
        if path in seen:
            continue
        seen.add(path)
        for name in sorted(imported_dlls(path)):
            if system_dll(name):
                continue
            if name not in available:
                raise ValueError(f"{path.name} imports missing non-system DLL {name}")
            dependencies[name] = available[name]
            pending.append(available[name])
    return dependencies


def copy_file(source, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)


def copy_runtime_data(prefix, bundle):
    schemas = bundle / "share/glib-2.0/schemas"
    schemas.mkdir(parents=True)
    inputs = list((prefix / "share/glib-2.0/schemas").glob("*.gschema.xml"))
    inputs = [p for p in inputs if not p.name.startswith("org.gtk.Demo")]
    if not inputs:
        raise ValueError(f"No GLib schemas found in {prefix}")
    for path in inputs:
        copy_file(path, schemas / path.name)
    for path in (prefix / "share/glib-2.0/schemas").glob("*.gschema.override"):
        copy_file(path, schemas / path.name)
    run(["glib-compile-schemas", "--strict", schemas])
    compiled = schemas / "gschemas.compiled"
    if not compiled.is_file() or not compiled.stat().st_size:
        raise ValueError("GLib did not produce compiled schemas")
    for path in schemas.iterdir():
        if path != compiled:
            path.unlink()
    portable = bundle / "share/discord-parcel"
    portable.mkdir(parents=True)
    (portable / "portable.marker").touch()
    copy_file(ROOT / "data/windows/loaders.cache.in", portable / "loaders.cache.in")

    adwaita = prefix / "share/icons/Adwaita"
    icon_dest = bundle / "share/icons/Adwaita"
    copy_file(adwaita / "index.theme", icon_dest / "index.theme")
    for name in ("symbolic", "scalable", "16x16"):
        source = adwaita / name
        if source.is_dir():
            shutil.copytree(source, icon_dest / name)
    hicolor = bundle / "share/icons/hicolor"
    copy_file(prefix / "share/icons/hicolor/index.theme", hicolor / "index.theme")
    copy_file(
        ROOT / "data/dev.akaduy.DiscordParcel.svg",
        hicolor / "scalable/apps/dev.akaduy.DiscordParcel.svg",
    )
    dictionary = prefix / "share/libthai/thbrk.tri"
    if dictionary.is_file():
        copy_file(dictionary, bundle / "share/libthai/thbrk.tri")


def copy_licenses(prefix, bundle, metadata):
    copy_file(ROOT / "LICENSE", bundle / "LICENSE")
    shutil.copytree(prefix / "share/licenses", bundle / "licenses/native")
    notices = [
        "Discord Parcel third-party components",
        "",
        "Adwaita icons: copyright GNOME Project; see licenses/native/adwaita-icon-theme.",
        "Native DLLs come from the supplied MSYS2 mingw64 runtime.",
        "Packages: https://repo.msys2.org/mingw/mingw64/",
        "Source/build recipes: https://github.com/msys2/MINGW-packages",
        "GStreamer: LGPL-2.1-or-later; https://gstreamer.freedesktop.org/src/",
        "See licenses/native/gtk4/COPYING for the LGPL text.",
        "The package maintainer must meet applicable source-distribution obligations.",
        "",
        "Rust dependency notices (including build-time dependencies):",
    ]
    for package in sorted(
        metadata["packages"], key=lambda p: (p["name"], p["version"])
    ):
        if package["source"] is None:
            continue
        name = f"{package['name']}-{package['version']}"
        source = Path(package["manifest_path"]).parent
        dest = bundle / "licenses/rust" / name
        notices.append(
            f"{name}: {package.get('license') or 'see license file'}; "
            f"{package.get('repository') or 'https://crates.io/crates/' + package['name']}"
        )
        files = {
            p
            for pattern in ("LICENSE*", "LICENCE*", "COPYING*", "NOTICE*")
            for p in source.glob(pattern)
        }
        if package.get("license_file"):
            files.add(source / package["license_file"])
        for path in sorted(files):
            if path.is_file():
                copy_file(path, dest / path.name)
            elif path.is_dir():
                shutil.copytree(path, dest / path.name, dirs_exist_ok=True)
    (bundle / "THIRD-PARTY-NOTICES.txt").write_text("\n".join(notices) + "\n")


def nsis_escape(value):
    if any(char in value for char in ("\n", "\r", "\0", '"')):
        raise ValueError(f"Unsupported NSIS path: {value!r}")
    return value.replace("$", "$$")


def uninstall_manifest(bundle):
    files = sorted(p.relative_to(bundle) for p in bundle.rglob("*") if p.is_file())
    directories = sorted(
        (p.relative_to(bundle) for p in bundle.rglob("*") if p.is_dir()),
        key=lambda p: (-len(p.parts), str(p)),
    )
    lines = []
    for command, paths in (("Delete", files), ("RMDir", directories)):
        for path in paths:
            for component in path.parts:
                if (
                    component in (".", "..")
                    or component.endswith((".", " "))
                    or any(ord(char) < 32 or char in '<>:"/\\|?*' for char in component)
                ):
                    raise ValueError(f"Unsafe Windows uninstall path: {path}")
            relative = nsis_escape(str(path).replace("/", "\\"))
            lines.append(f'{command} "$INSTDIR\\{relative}"')
    return "\n".join(lines) + "\n"


def create_portable_archive(bundle, work, version):
    """Add empty local-data directories to the freshly staged runtime and zip it."""
    for name in ("downloads", "locks", "sent", "uploads"):
        (bundle / name).mkdir()
    zip_epoch = datetime(1980, 1, 1).timestamp()
    for path in bundle.rglob("*"):
        stat = path.stat()
        if stat.st_mtime < zip_epoch:
            os.utime(path, (stat.st_atime, zip_epoch))
    return Path(
        shutil.make_archive(
            str(work / f"discord-parcel-{version}-windows-x64-Portable"),
            "zip",
            root_dir=bundle.parent,
            base_dir=bundle.name,
        )
    )


def nsis_command():
    env = os.environ.copy()
    compiler = shutil.which("makensis")
    if not compiler:
        local = ROOT / ".win/nsis/usr/bin/makensis"
        if not local.is_file():
            raise ValueError(
                "Install NSIS (makensis), or provide .win/nsis/usr/bin/makensis"
            )
        compiler = str(local)
        env["NSISDIR"] = str(ROOT / ".win/nsis/usr/share/nsis")
    return compiler, env


def build_environment(prefix):
    env = os.environ.copy()
    suffix = TARGET.replace("-", "_")
    env[f"PKG_CONFIG_{suffix}"] = "pkg-config"
    env[f"PKG_CONFIG_ALLOW_CROSS_{suffix}"] = "1"
    env[f"PKG_CONFIG_SYSROOT_DIR_{suffix}"] = str(prefix.parent)
    env[f"PKG_CONFIG_LIBDIR_{suffix}"] = os.pathsep.join(
        str(prefix / path) for path in ("lib/pkgconfig", "share/pkgconfig")
    )
    env[f"PKG_CONFIG_PATH_{suffix}"] = ""
    env[f"CARGO_TARGET_{suffix.upper()}_LINKER"] = "x86_64-w64-mingw32-gcc"
    env["CARGO_TARGET_DIR"] = str(ROOT / "target")
    return env


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--prefix",
        type=Path,
        default=ROOT / ".win/sysroot/mingw64",
        help="existing MSYS2 mingw64 prefix (default: .win/sysroot/mingw64)",
    )
    parser.add_argument(
        "--skip-build", action="store_true", help="package the existing release EXE"
    )
    args = parser.parse_args()
    prefix = args.prefix.resolve()
    compiler, compiler_env = nsis_command()
    for tool in ("cargo", "glib-compile-schemas", "x86_64-w64-mingw32-strip"):
        if not shutil.which(tool):
            raise ValueError(f"Required build tool not found: {tool}")
    env = build_environment(prefix)
    if not args.skip_build:
        run(
            [
                "cargo",
                "build",
                "--release",
                "--locked",
                "--offline",
                "--target",
                TARGET,
            ],
            cwd=ROOT,
            env=env,
        )
    exe = ROOT / "target" / TARGET / "release/discord-parcel.exe"
    metadata = json.loads(
        run(
            [
                "cargo",
                "metadata",
                "--locked",
                "--offline",
                "--format-version",
                "1",
                "--filter-platform",
                TARGET,
            ],
            cwd=ROOT,
            env=env,
            capture_output=True,
            text=True,
        ).stdout
    )
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
    dependencies = dependency_closure([exe, prefix / SVG_MODULE], prefix / "bin")
    dist = ROOT / "dist"
    dist.mkdir(exist_ok=True)
    name = f"discord-parcel-{version}-windows-x64-Setup"
    with tempfile.TemporaryDirectory(prefix=".windows-setup-", dir=dist) as temporary:
        work = Path(temporary)
        bundle = work / "DiscordParcel"
        bundle.mkdir()
        copy_file(exe, bundle / exe.name)
        for path in dependencies.values():
            copy_file(path, bundle / path.name)
        copy_file(prefix / SVG_MODULE, bundle / SVG_MODULE)
        binaries = [bundle / exe.name, bundle / SVG_MODULE]
        binaries.extend(bundle / path.name for path in dependencies.values())
        for path in binaries:
            before = imported_dlls(path)
            subprocess.run(
                ["x86_64-w64-mingw32-strip", "--strip-unneeded", str(path)], check=True
            )
            if imported_dlls(path) != before:
                raise ValueError(f"Stripping changed dependencies of {path.name}")
        copy_runtime_data(prefix, bundle)
        copy_licenses(prefix, bundle, metadata)
        dependency_closure([bundle / exe.name, bundle / SVG_MODULE], bundle)
        files = [
            {
                "path": p.relative_to(bundle).as_posix(),
                "bytes": p.stat().st_size,
                "sha256": hashlib.sha256(p.read_bytes()).hexdigest(),
            }
            for p in sorted(bundle.rglob("*"))
            if p.is_file()
        ]
        installed_size = sum(item["bytes"] for item in files)
        uninstall = work / "uninstall-files.nsh"
        uninstall.write_text(uninstall_manifest(bundle))
        output = work / f"{name}.exe"
        run(
            [
                compiler,
                "-V2",
                f"-DAPP_VERSION={version}",
                f"-DBUNDLE_DIR={bundle}",
                f"-DOUTPUT_FILE={output}",
                f"-DAPP_ICON={ROOT / 'data/windows/app.ico'}",
                f"-DINSTALL_FILES={uninstall}",
                ROOT / "data/windows/installer.nsi",
            ],
            env=compiler_env,
            timeout=240,
        )
        manifest = work / f"{name}.manifest.json"
        manifest.write_text(
            json.dumps(
                {
                    "version": version,
                    "dll_count": len(dependencies) + 1,
                    "installed_bytes": installed_size,
                    "files": files,
                },
                indent=2,
            )
            + "\n"
        )
        portable = create_portable_archive(bundle, work, version)
        output.replace(dist / output.name)
        manifest.replace(dist / manifest.name)
        portable.replace(dist / portable.name)
    result = dist / f"{name}.exe"
    portable_result = dist / portable.name
    print(
        f"\nInstaller: {result}\n"
        f"Installer download: {result.stat().st_size / 1024**2:.1f} MiB\n"
        f"Portable: {portable_result}\n"
        f"Portable download: {portable_result.stat().st_size / 1024**2:.1f} MiB\n"
        f"Installed files: {installed_size / 1024**2:.1f} MiB\n"
        f"Runtime DLLs/modules: {len(dependencies) + 1}"
    )


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"Windows packaging failed: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr, file=sys.stderr)
        sys.exit(1)
