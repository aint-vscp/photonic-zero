# Puts the Rust toolchain and a mingw-w64 linker on PATH for the current shell.
#
#   . .\scripts\dev-env.ps1
#
# Only needed on Windows machines where rustup was installed with
# --no-modify-path, or where the C toolchain lives in a winget package
# directory. On Linux and macOS the usual `rustup` PATH setup is enough.

$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
if (Test-Path $cargoBin) {
    $env:PATH = "$cargoBin;$env:PATH"
} else {
    Write-Warning "cargo not found at $cargoBin - install from https://rustup.rs"
}

# Presence of a `gcc` on PATH is not enough: a stale 32-bit MinGW.org install
# will happily shadow the 64-bit toolchain and then fail at link time with
# "cannot find -lbcrypt". Check what the compiler actually targets.
function Test-Gcc64 {
    $cmd = Get-Command gcc.exe -ErrorAction SilentlyContinue
    if (-not $cmd) { return $false }
    try { return ((& $cmd.Source -dumpmachine 2>$null) -match "x86_64") } catch { return $false }
}

if (-not (Test-Gcc64)) {
    $gcc = Get-ChildItem "$env:LOCALAPPDATA\Microsoft\WinGet\Packages" -Recurse -Filter "gcc.exe" -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match "mingw64" } |
        Select-Object -First 1
    if ($gcc) {
        $env:PATH = "$($gcc.DirectoryName);$env:PATH"
    } else {
        Write-Warning "no 64-bit mingw-w64 gcc found; x86_64-pc-windows-gnu needs one"
        Write-Warning "install with: winget install -e --id BrechtSanders.WinLibs.POSIX.MSVCRT"
    }
}

Write-Host "photonic-zero dev environment ready"
if (Get-Command rustc -ErrorAction SilentlyContinue) { rustc --version }
if (Get-Command gcc -ErrorAction SilentlyContinue) { (gcc --version | Select-Object -First 1) }
