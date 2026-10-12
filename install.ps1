# slick installer for Windows.
#
#   irm https://slick.leonavas.com/install.ps1 | iex
#
# Downloads the latest release (or $env:SLICK_VERSION = 'v0.2.0'), checks its
# SHA-256, unpacks slick.exe into %LOCALAPPDATA%\Programs\slick and adds that
# folder to your user PATH. No admin. Installed there, slick updates itself.
#
# This file is the source; landing/public/install.ps1 is an identical copy
# served by the site. Edit this one, then copy it over.

# In a script block, so nothing here leaks into the caller's session.
& {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    # Windows PowerShell 5.1 may not offer TLS 1.2 by default; GitHub needs it.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $repo = 'leonavas/slick'

    function Fail($msg) { throw "slick: error: $msg" }

    $arch = $env:PROCESSOR_ARCHITECTURE
    if ($env:PROCESSOR_ARCHITEW6432) { $arch = $env:PROCESSOR_ARCHITEW6432 }
    switch ($arch) {
        'AMD64' { $target = 'x86_64-pc-windows-msvc' }
        'ARM64' { Fail 'Windows on ARM is not supported yet (only x86_64).' }
        default { Fail "unsupported architecture: $arch" }
    }

    $tag = $env:SLICK_VERSION
    if (-not $tag) {
        try {
            $tag = (Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest" -UseBasicParsing).tag_name
        } catch {
            Fail 'could not find the latest release on GitHub'
        }
    }
    if (-not $tag.StartsWith('v')) { $tag = "v$tag" }
    $ver = $tag.Substring(1)

    $name = "slick-$ver-$target.zip"
    $url = "https://github.com/$repo/releases/download/$tag/$name"
    $dir = Join-Path $env:LOCALAPPDATA 'Programs\slick'
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("slick-" + [Guid]::NewGuid())
    New-Item -ItemType Directory -Path $tmp | Out-Null

    try {
        Write-Host "slick: downloading slick $ver ($target)"
        $zip = Join-Path $tmp $name
        try {
            Invoke-WebRequest $url -OutFile $zip -UseBasicParsing
            Invoke-WebRequest "$url.sha256" -OutFile "$zip.sha256" -UseBasicParsing
        } catch {
            Fail "download failed: $url"
        }

        $want = ((Get-Content "$zip.sha256" -Raw).Trim() -split '\s+')[0].ToLower()
        $got = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
        if ($want -ne $got) { Fail "SHA-256 mismatch ($got, expected $want)" }

        Expand-Archive $zip -DestinationPath (Join-Path $tmp 'out') -Force
        New-Item -ItemType Directory -Path $dir -Force | Out-Null
        $exe = Join-Path $dir 'slick.exe'
        # A running slick.exe can't be overwritten, only renamed.
        if (Test-Path $exe) {
            $old = Join-Path $dir 'slick.old.exe'
            Remove-Item $old -Force -ErrorAction SilentlyContinue
            Move-Item $exe $old -Force
        }
        Move-Item (Join-Path $tmp 'out\slick.exe') $exe -Force
        Write-Host "slick: installed $exe"
    } finally {
        Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }

    $path = [Environment]::GetEnvironmentVariable('Path', 'User')
    $parts = @($path -split ';' | Where-Object { $_ })
    if ($parts -notcontains $dir) {
        [Environment]::SetEnvironmentVariable('Path', (($parts + $dir) -join ';'), 'User')
        Write-Host "slick: added $dir to your user PATH"
    }
    if (($env:Path -split ';') -notcontains $dir) { $env:Path = "$env:Path;$dir" }

    Write-Host 'slick: done. Open a new terminal and run: slick login' -ForegroundColor Green
}
