$ErrorActionPreference = 'Stop'

Set-Location (Join-Path $PSScriptRoot '..')
$target = 'x86_64-pc-windows-msvc'
$curlHash = '8D28C1093E0B6345917D2C1710C67F78F61834D76EF983EA9FB631C75E20312F'

if ((rustc --version) -notmatch '^rustc 1\.97\.1 ') {
    throw 'Rust 1.97.1 is required.'
}
if ((Get-FileHash -Algorithm SHA256 -LiteralPath 'assets/curl.exe').Hash.ToUpperInvariant() -ne $curlHash) {
    throw 'assets/curl.exe hash mismatch.'
}

cargo fmt --check
if ($LASTEXITCODE -ne 0) {
    throw "cargo fmt failed with exit code $LASTEXITCODE."
}
cargo clippy --all-targets --target $target -- -D warnings
if ($LASTEXITCODE -ne 0) {
    throw "cargo clippy failed with exit code $LASTEXITCODE."
}
cargo test --target $target
if ($LASTEXITCODE -ne 0) {
    throw "cargo test failed with exit code $LASTEXITCODE."
}
cargo build --release --target $target
if ($LASTEXITCODE -ne 0) {
    throw "cargo build failed with exit code $LASTEXITCODE."
}

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
