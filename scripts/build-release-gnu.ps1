$ErrorActionPreference = 'Stop'

Set-Location (Join-Path $PSScriptRoot '..')
$toolchain = 'stable-x86_64-pc-windows-gnu'
$target = 'x86_64-pc-windows-gnu'
$curlHash = '8D28C1093E0B6345917D2C1710C67F78F61834D76EF983EA9FB631C75E20312F'

function Invoke-CargoChecked {
    param([string[]]$Arguments)
    $previousToolchain = [Environment]::GetEnvironmentVariable('RUSTUP_TOOLCHAIN', 'Process')
    $env:RUSTUP_TOOLCHAIN = $toolchain
    try {
        & cargo @Arguments
        $exitCode = $LASTEXITCODE
    } finally {
        if ($null -eq $previousToolchain) {
            Remove-Item Env:RUSTUP_TOOLCHAIN -ErrorAction SilentlyContinue
        } else {
            $env:RUSTUP_TOOLCHAIN = $previousToolchain
        }
    }
    if ($exitCode -ne 0) { throw "cargo $($Arguments -join ' ') failed with exit code $exitCode." }
}

& rustup run $toolchain rustc --version
if ($LASTEXITCODE -ne 0) { throw "Rust GNU toolchain '$toolchain' is required." }
if ((Get-FileHash -Algorithm SHA256 -LiteralPath 'assets/curl.exe').Hash.ToUpperInvariant() -ne $curlHash) {
    throw 'assets/curl.exe hash mismatch.'
}

Invoke-CargoChecked @('fmt', '--', '--check')
Invoke-CargoChecked @('clippy', '--ignore-rust-version', '--all-targets', '--target', $target, '--', '-D', 'warnings')
Invoke-CargoChecked @('test', '--ignore-rust-version', '--target', $target, '--', '--test-threads=1')
Invoke-CargoChecked @('build', '--ignore-rust-version', '--release', '--target', $target, '--bin', 'curl-downloader')

# The smoke probe needs a controlled parent-process authentication override.
# Keep that test-only feature in a separate optimized target directory so the
# shipped, feature-free release binary is never replaced by the probe build.
$smokeTargetDirectory = 'target/smoke-native-auth'
Invoke-CargoChecked @('--target-dir', $smokeTargetDirectory, 'build', '--ignore-rust-version', '--release', '--features', 'smoke-test-native-auth', '--target', $target, '--bin', 'curl-downloader')

New-Item -ItemType Directory -Path 'dist' -Force | Out-Null
Copy-Item -LiteralPath "target/$target/release/curl-downloader.exe" -Destination 'dist/CurlDownloader.exe'

$scriptHost = (Get-Command pwsh -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty Source)
if ([string]::IsNullOrWhiteSpace($scriptHost)) {
    $scriptHost = (Get-Command powershell).Source
}
& $scriptHost -NoProfile -ExecutionPolicy Bypass -File 'scripts/test-minimized-background-controller.ps1' -ExecutablePath 'dist/CurlDownloader.exe' -SkipNativeMessaging
if ($LASTEXITCODE -ne 0) {
    throw 'Release GUI smoke test failed.'
}
$smokeExecutable = Join-Path $smokeTargetDirectory "$target\release\curl-downloader.exe"
& $scriptHost -NoProfile -ExecutionPolicy Bypass -File 'scripts/test-minimized-background-controller.ps1' -ExecutablePath $smokeExecutable
if ($LASTEXITCODE -ne 0) {
    throw 'Minimized background controller smoke test failed.'
}

$executables = @(Get-ChildItem -LiteralPath 'dist' -Filter '*.exe' -File)
if ($executables.Count -ne 1 -or $executables[0].Name -ne 'CurlDownloader.exe') {
    throw 'Release must contain exactly one CurlDownloader.exe.'
}
Get-FileHash -Algorithm SHA256 -LiteralPath 'dist/CurlDownloader.exe'
