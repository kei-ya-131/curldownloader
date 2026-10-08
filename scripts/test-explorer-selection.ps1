param([string]$CargoToolchain = '+stable-x86_64-pc-windows-gnu')
$ErrorActionPreference = 'Stop'
$workspace = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$fixture = Join-Path $workspace ('target\explorer-selection-' + [Guid]::NewGuid().ToString('N'))
$shell = New-Object -ComObject Shell.Application
$beforeHandles = @($shell.Windows() | ForEach-Object { $_.HWND })
New-Item -ItemType Directory -Path $fixture | Out-Null
$previousFile = $env:CURL_DOWNLOADER_REVEAL_TEST_FILE
try {
    foreach ($name in @('first file.bin', '中文,測試.bin')) {
        $file = Join-Path $fixture $name
        [IO.File]::WriteAllBytes($file, [byte[]](1, 2, 3))
        $env:CURL_DOWNLOADER_REVEAL_TEST_FILE = $file
        & cargo $CargoToolchain test --ignore-rust-version --target x86_64-pc-windows-gnu --lib reveal_selects_fixture_in_explorer -- --ignored --test-threads=1
        if ($LASTEXITCODE -ne 0) { throw 'Native reveal test failed' }
        $deadline = [DateTime]::UtcNow.AddSeconds(5)
        do {
            $selected = @($shell.Windows() | ForEach-Object {
                try {
                    if ($_.Document.Folder.Self.Path -eq $fixture) {
                        $_.Document.SelectedItems() | ForEach-Object { $_.Path }
                    }
                } catch { }
            })
            if ($selected -contains $file) { break }
            Start-Sleep -Milliseconds 100
        } while ([DateTime]::UtcNow -lt $deadline)
        if ($selected -notcontains $file) { throw "Explorer did not select $name" }
        Write-Output "Explorer selected: $name"
    }
} finally {
    $env:CURL_DOWNLOADER_REVEAL_TEST_FILE = $previousFile
    # Close only a newly created window that still shows our fixture folder.
    foreach ($window in $shell.Windows()) {
        try {
            if ($beforeHandles -notcontains $window.HWND -and $window.Document.Folder.Self.Path -eq $fixture) {
                $window.Quit()
            }
        } catch { }
    }
    if ([IO.Path]::GetFullPath($fixture).StartsWith($workspace + '\', [StringComparison]::OrdinalIgnoreCase)) {
        Remove-Item -LiteralPath $fixture -Recurse -Force
    }
}
