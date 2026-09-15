# Installer for the Drop command-line client on Windows.
#
#   irm https://github.com/op-q/drop/releases/latest/download/install.ps1 | iex
#
# Downloads a prebuilt drop.exe from the project's GitHub releases, verifies it
# against the published checksums, installs it, and puts it on the user's PATH.
# The same three variables as install.sh:
#
#   DROP_INSTALL_DIR   where drop.exe lands (default: %LOCALAPPDATA%\Programs\drop)
#   DROP_VERSION       release tag to install (default: latest)
#   DROP_RELEASE_BASE  base URL holding the release assets, for self-hosted
#                      mirrors of the published binaries
#
# Nothing here needs administrator rights. The PATH change is the user's own.
#
# Wrapped in a script block so that, run through `iex`, nothing here (the
# preference variables, the functions) leaks into the session that ran it.

& {
    $ErrorActionPreference = 'Stop'
    # Windows PowerShell 5 negotiates TLS 1.0 by default, which GitHub refuses.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    # The progress bar makes Invoke-WebRequest many times slower in Windows
    # PowerShell 5.
    $ProgressPreference = 'SilentlyContinue'

    $Repo = 'op-q/drop'

    # Throws rather than exits. Run as `irm ... | iex`, this script executes inside
    # the user's own PowerShell session, and `exit` would close their window.
    function Fail([string] $Message) {
        throw "error: $Message"
    }

    function Get-Target {
        # PROCESSOR_ARCHITEW6432 is set when a 32-bit PowerShell runs on a 64-bit
        # system, and names the real architecture.
        $arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }

        switch ($arch) {
            'AMD64' { return 'x86_64-pc-windows-msvc' }
            'ARM64' { return 'aarch64-pc-windows-msvc' }
            default { Fail "unsupported architecture: $arch. Build from source with: cargo install --git https://github.com/$Repo drop-cli" }
        }
    }

    $version = if ($env:DROP_VERSION) { $env:DROP_VERSION } else { 'latest' }
    $installDir = if ($env:DROP_INSTALL_DIR) { $env:DROP_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\drop' }

    if ($env:DROP_RELEASE_BASE) {
        $base = $env:DROP_RELEASE_BASE.TrimEnd('/')
    } elseif ($version -eq 'latest') {
        $base = "https://github.com/$Repo/releases/latest/download"
    } else {
        $base = "https://github.com/$Repo/releases/download/$version"
    }

    $target = Get-Target
    $archive = "drop-$target.zip"

    $work = Join-Path ([IO.Path]::GetTempPath()) ("drop-install-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $work | Out-Null

    try {
        Write-Host "Downloading drop for $target..."
        try {
            Invoke-WebRequest -UseBasicParsing -Uri "$base/$archive" -OutFile (Join-Path $work $archive)
        } catch {
            Fail "could not download $base/$archive"
        }
        try {
            Invoke-WebRequest -UseBasicParsing -Uri "$base/checksums.txt" -OutFile (Join-Path $work 'checksums.txt')
        } catch {
            Fail 'could not download the checksums file'
        }

        # checksums.txt is sha256sum output: "<hex>  <name>", or "<hex> *<name>".
        $expected = $null
        foreach ($line in Get-Content (Join-Path $work 'checksums.txt')) {
            $fields = $line -split '\s+', 2
            if ($fields.Count -eq 2 -and $fields[1].TrimStart('*') -eq $archive) {
                $expected = $fields[0].ToLowerInvariant()
            }
        }
        if (-not $expected) { Fail "no checksum published for $archive" }

        $actual = (Get-FileHash -Algorithm SHA256 -Path (Join-Path $work $archive)).Hash.ToLowerInvariant()
        if ($expected -ne $actual) { Fail "checksum mismatch for ${archive}: expected $expected, got $actual" }
        Write-Host 'Checksum verified.'

        $unpacked = Join-Path $work 'unpacked'
        Expand-Archive -Path (Join-Path $work $archive) -DestinationPath $unpacked
        $binary = Join-Path $unpacked 'drop.exe'
        if (-not (Test-Path $binary)) { Fail 'the downloaded archive did not contain drop.exe' }

        New-Item -ItemType Directory -Force -Path $installDir | Out-Null
        $destination = Join-Path $installDir 'drop.exe'

        # A running drop.exe cannot be overwritten, but it can be renamed. Moving
        # the old one aside first lets an upgrade succeed while a transfer is still
        # using it; the leftover is removed on the next install.
        $previous = "$destination.old"
        Remove-Item -Force -ErrorAction SilentlyContinue $previous
        if (Test-Path $destination) { Move-Item -Force $destination $previous }
        Copy-Item -Force $binary $destination
        Remove-Item -Force -ErrorAction SilentlyContinue $previous

        Write-Host ''
        Write-Host "Installed drop to $destination"

        # The user's PATH, not the machine's: no elevation, and nobody else's
        # environment changes.
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $entries = if ($userPath) { $userPath -split ';' } else { @() }
        if ($entries -notcontains $installDir) {
            $updated = (@($entries | Where-Object { $_ }) + $installDir) -join ';'
            [Environment]::SetEnvironmentVariable('Path', $updated, 'User')
            Write-Host ''
            Write-Host "Added $installDir to your PATH. Open a new terminal to use drop."
        }

        Write-Host ''
        Write-Host 'Send a file or folder:   drop send .\some-folder'
        Write-Host 'Receive it elsewhere:    drop recv <CODE>'
    } finally {
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $work
    }
}
