# Builds the ELF QEMU wants and the flat image the board's firmware loads.
$ErrorActionPreference = "Stop"
$crate = Split-Path -Parent $MyInvocation.MyCommand.Path
$root = Split-Path -Parent $crate
$objcopy = Join-Path $env:USERPROFILE ".rustup\toolchains\nightly-2026-10-05-x86_64-pc-windows-msvc\lib\rustlib\x86_64-pc-windows-msvc\bin\rust-objcopy.exe"
Push-Location $root
try
{
    cargo build --manifest-path (Join-Path $crate "Cargo.toml") --release --target aarch64-unknown-none
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    $elf = Join-Path $crate "target\aarch64-unknown-none\release\selftest-aarch64"
    $image = Join-Path $crate "kernel8.img"
    & $objcopy -O binary $elf $image
    if ($LASTEXITCODE -ne 0) { throw "rust-objcopy failed" }
    $head = [System.IO.File]::ReadAllBytes($image)[0..3]
    if ($head[0] -eq 0x7f -and $head[1] -eq 0x45 -and $head[2] -eq 0x4c -and $head[3] -eq 0x46)
    {
        throw "kernel8.img is still an ELF; the firmware would load it raw and not start"
    }
    Write-Output ("kernel8.img {0} bytes, first bytes {1}" -f (Get-Item $image).Length,
                  (($head | ForEach-Object { $_.ToString("x2") }) -join " "))
}
finally
{
    Pop-Location
}
