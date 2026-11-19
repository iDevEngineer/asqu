# Installs Asqu on Windows, the version control that nobody has to manage.
#
#   irm https://asqu.dev/install.ps1 | iex
#
# Downloads the Windows build, checks its fingerprint (SHA-256), puts asqu.exe in
# %LOCALAPPDATA%\Programs\Asqu and adds that folder to your PATH (for you only).
# Nothing else on the system is changed.
#
# Settings (all optional):
#   $env:ASQU_INSTALL_DIR    where to put the program
#   $env:ASQU_DOWNLOAD_BASE  download from a mirror (also used for testing)

& {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'  # the progress bar makes downloads very slow

    $base = if ($env:ASQU_DOWNLOAD_BASE) { $env:ASQU_DOWNLOAD_BASE } else { 'https://asqu.dev/download' }
    $dest = if ($env:ASQU_INSTALL_DIR) { $env:ASQU_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\Asqu' }
    $asset = 'asqu-x86_64-pc-windows-msvc.zip'

    # Works in every PowerShell, including Windows PowerShell 5.1. A 32-bit PowerShell on
    # a 64-bit Windows reports the real processor in PROCESSOR_ARCHITEW6432.
    $arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    if ($arch -ne 'AMD64' -and $arch -ne 'ARM64') {
        Write-Host "asqu install: this processor ($arch) isn't supported yet" -ForegroundColor Red
        return
    }

    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("asqu-install-" + [guid]::NewGuid())
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        # Older Windows PowerShell may not use TLS 1.2 by default; asqu.dev needs it.
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
        Write-Host "Downloading Asqu for Windows..."
        $zip = Join-Path $tmp $asset
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile $zip
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset.sha256" -OutFile "$zip.sha256"

        $want = ((Get-Content "$zip.sha256" -Raw).Trim() -split '\s+')[0].ToLowerInvariant()
        $got = (Get-FileHash -Algorithm SHA256 -Path $zip).Hash.ToLowerInvariant()
        if ($want -ne $got) {
            throw "asqu install: the download doesn't match its fingerprint, so it wasn't installed"
        }

        Expand-Archive -Path $zip -DestinationPath (Join-Path $tmp 'x') -Force
        $new = Join-Path $tmp 'x\asqu.exe'
        if (-not (Test-Path $new)) { throw "asqu install: the download didn't contain asqu.exe" }

        New-Item -ItemType Directory -Force -Path $dest | Out-Null
        $exe = Join-Path $dest 'asqu.exe'
        if (Test-Path $exe) {
            # A running asqu.exe can't be overwritten, but it can be moved aside.
            $old = Join-Path $dest 'asqu.old.exe'
            Remove-Item -Force -ErrorAction SilentlyContinue $old
            Move-Item -Force $exe $old
        }
        Move-Item -Force $new $exe
        $version = & $exe --version
        Write-Host "Installed $version to $exe"
        Write-Host "Asqu is free to use under its licence: https://asqu.dev/license"

        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $parts = if ($userPath) { $userPath -split ';' } else { @() }
        if ($parts -notcontains $dest) {
            [Environment]::SetEnvironmentVariable('Path', (($parts + $dest) | Where-Object { $_ }) -join ';', 'User')
            $env:Path = "$env:Path;$dest"
            Write-Host "Added $dest to your PATH. Open a new terminal for other windows to see it."
        }

        Write-Host ""
        Write-Host "Next steps:"
        Write-Host "  1. Go to a project folder:   cd path\to\your\project"
        Write-Host "  2. If you use AI tools:      asqu setup"
        Write-Host "  3. See your history:         asqu open"
    }
    catch {
        # Report the problem without closing the PowerShell window.
        Write-Host $_.Exception.Message -ForegroundColor Red
    }
    finally {
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $tmp
    }
}
