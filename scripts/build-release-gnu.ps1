$ErrorActionPreference = 'Stop'

Set-Location (Join-Path $PSScriptRoot '..')
$toolchain = 'stable-x86_64-pc-windows-gnu'
$target = 'x86_64-pc-windows-gnu'
$curlHash = '8D28C1093E0B6345917D2C1710C67F78F61834D76EF983EA9FB631C75E20312F'

function Invoke-CargoChecked {
    param([string[]]$Arguments)

    & rustup run $toolchain cargo @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "cargo $($Arguments -join ' ') failed with exit code $LASTEXITCODE."
    }
}

& rustup run $toolchain rustc --version
if ($LASTEXITCODE -ne 0) {
    throw "Rust GNU toolchain '$toolchain' is required."
}
if ((Get-FileHash -Algorithm SHA256 -LiteralPath 'assets/curl.exe').Hash.ToUpperInvariant() -ne $curlHash) {
    throw 'assets/curl.exe hash mismatch.'
}

Invoke-CargoChecked @('fmt', '--', '--check')
Invoke-CargoChecked @('clippy', '--ignore-rust-version', '--all-targets', '--target', $target, '--', '-D', 'warnings')
Invoke-CargoChecked @('test', '--ignore-rust-version', '--target', $target, '--', '--test-threads=1')
Invoke-CargoChecked @('build', '--ignore-rust-version', '--release', '--target', $target)

if (Test-Path -LiteralPath 'dist') {
    Remove-Item -LiteralPath 'dist' -Recurse -Force
}
New-Item -ItemType Directory -Path 'dist' | Out-Null
Copy-Item -LiteralPath "target/$target/release/curl-downloader.exe" -Destination 'dist/CurlDownloader.exe'

$files = @(Get-ChildItem -LiteralPath 'dist' -File)
if ($files.Count -ne 1 -or $files[0].Name -ne 'CurlDownloader.exe') {
    throw 'Release must contain exactly one EXE.'
}
Get-FileHash -Algorithm SHA256 -LiteralPath 'dist/CurlDownloader.exe'
