# Install Reformed Agent (hermes-agent-ultra) on 64-bit Windows from GitHub Releases.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Position = 0)]
    [string]$Version = $(if ($env:HERMES_VERSION) { $env:HERMES_VERSION } elseif ($env:VERSION) { $env:VERSION } else { 'latest' }),
    [string]$InstallDir = $(if ($env:HERMES_INSTALL_DIR) { $env:HERMES_INSTALL_DIR } elseif ($env:INSTALL_DIR) { $env:INSTALL_DIR } else { '' }),
    [string]$HermesHome = $(if ($env:HERMES_HOME) { $env:HERMES_HOME } elseif ($env:HERMES_AGENT_ULTRA_HOME) { $env:HERMES_AGENT_ULTRA_HOME } else { '' }),
    [string]$Repo = $(if ($env:REPO) { $env:REPO } else { 'yungSNAT/reformed-agent' }),
    [string]$ReleaseBaseUrl = $(if ($env:HERMES_RELEASE_BASE_URL) { $env:HERMES_RELEASE_BASE_URL } else { '' }),
    [switch]$LegacyAlias,
    [switch]$NoModifyPath,
    [switch]$Setup,
    [switch]$SkipSetup,
    [switch]$Help
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$script:DefaultSoulMd = "You are Hermes Agent, an intelligent AI assistant created by Nous Research. You are helpful, knowledgeable, and direct. You assist users with a wide range of tasks including answering questions, writing and editing code, analyzing information, creative work, and executing actions via your tools. You communicate clearly, admit uncertainty when appropriate, and prioritize being genuinely useful over being verbose unless otherwise directed below. Be targeted and efficient in your exploration and investigations. When the user asks for an actionable task, execute immediately: run the first concrete step with available tools, then continue until completion. Do not stop at intent-only narration such as 'I'll proceed' without performing work."

function Show-Usage {
    @'
Usage: scripts/install.ps1 [-Version TAG] [-InstallDir PATH] [-HermesHome PATH] [options]

Install Reformed Agent (hermes-agent-ultra) on Windows (x86_64) from GitHub Releases.
Downloads hermes-windows-x86_64.zip and verifies SHA256SUMS (plus its cosign signature
when cosign is on PATH) before extracting.

Options:
  -Version TAG           Release tag to install (default: latest)
  -InstallDir PATH       Destination folder (default: %LOCALAPPDATA%\Programs\ReformedAgent)
  -HermesHome PATH       Hermes home directory for SOUL.md bootstrap (default: ~\.hermes-agent-ultra)
  -LegacyAlias           Also install legacy hermes.exe alias (default: off)
  -NoModifyPath          Do not add -InstallDir to user PATH
  -Setup                 Run post-install verification probes (doctor, auth status)
  -SkipSetup             Skip post-install verification and setup prompt
  -Help                  Show this help

Environment variables:
  REPO                   GitHub repo slug (default: yungSNAT/reformed-agent)
  HERMES_VERSION         Release tag override (same as -Version)
  HERMES_INSTALL_DIR     Destination folder override (same as -InstallDir)
  HERMES_HOME            Config dir override (default: $HERMES_AGENT_ULTRA_HOME, else ~\.hermes-agent-ultra)
  HERMES_RELEASE_BASE_URL
                         HTTPS folder URL (or loopback HTTP for tests) holding release assets
  HERMES_REQUIRE_SIGNATURE
                         Refuse to install unless cosign verifies SHA256SUMS (default: false)
  HERMES_NO_MODIFY_PATH  Set to 1/true to skip modifying user PATH
  INSTALL_LEGACY_ALIAS   Set to 1/true to also install hermes.exe
'@
}

function Test-Truthy {
    param([string]$Value)
    return $Value -in @('1', 'true', 'TRUE', 'True', 'yes', 'YES', 'Yes', 'on', 'ON', 'On')
}

function Test-TrustedUri {
    param(
        [Uri]$Uri,
        [Uri]$LoopbackBase
    )
    if ($null -eq $Uri -or -not $Uri.IsAbsoluteUri) { return $false }
    if ($Uri.Scheme -eq 'https') { return $true }
    if ($null -ne $LoopbackBase -and $LoopbackBase.Scheme -eq 'http' -and $LoopbackBase.IsLoopback) {
        return $Uri.Scheme -eq 'http' -and $Uri.IsLoopback -and $Uri.Authority -eq $LoopbackBase.Authority
    }
    return $false
}

function Get-ResponseUri {
    param($Response)
    if ($null -eq $Response -or $null -eq $Response.BaseResponse) { return $null }
    if ($Response.BaseResponse.ResponseUri) { return $Response.BaseResponse.ResponseUri }
    if ($Response.BaseResponse.RequestMessage -and $Response.BaseResponse.RequestMessage.RequestUri) {
        return $Response.BaseResponse.RequestMessage.RequestUri
    }
    return $null
}

function Invoke-VerifiedDownload {
    param(
        [Parameter(Mandatory = $true)][string]$Url,
        [Parameter(Mandatory = $true)][string]$OutFile,
        [Uri]$LoopbackBase
    )
    $targetUri = $null
    if (-not [Uri]::TryCreate($Url, [UriKind]::Absolute, [ref]$targetUri) -or -not (Test-TrustedUri -Uri $targetUri -LoopbackBase $LoopbackBase)) {
        throw "Refusing to install: untrusted download URI '$Url' (HTTPS required)."
    }
    try {
        $response = Invoke-WebRequest -Uri $targetUri -OutFile $OutFile -TimeoutSec 120 -MaximumRedirection 5 -UseBasicParsing -PassThru
    } catch {
        throw "Refusing to install: failed downloading $Url ($($_.Exception.Message))."
    }
    $finalUri = Get-ResponseUri -Response $response
    if (-not (Test-TrustedUri -Uri $finalUri -LoopbackBase $LoopbackBase)) {
        throw "Refusing to install: '$Url' redirected to untrusted URI '$finalUri'."
    }
}

function Resolve-LatestTag {
    param([Parameter(Mandatory = $true)][string]$RepoSlug)
    $latestUrl = "https://github.com/$RepoSlug/releases/latest"
    try {
        $response = Invoke-WebRequest -Method Head -Uri $latestUrl -TimeoutSec 60 -MaximumRedirection 5 -UseBasicParsing
    } catch {
        throw "Refusing to install: could not resolve latest release tag for $RepoSlug ($($_.Exception.Message))."
    }
    $finalUri = Get-ResponseUri -Response $response
    if (-not (Test-TrustedUri -Uri $finalUri -LoopbackBase $null) -or $finalUri.Host -ne 'github.com') {
        throw "Refusing to install: unexpected redirect target '$finalUri' resolving latest release."
    }
    if ($finalUri.AbsolutePath -match '/releases/tag/([^/?#]+)$') {
        $tag = $Matches[1]
        if ($tag -match '^[A-Za-z0-9][A-Za-z0-9._+-]*$') {
            return $tag
        }
    }
    throw "Refusing to install: could not extract valid release tag from '$finalUri'."
}

function Get-ExpectedAssetHash {
    param(
        [Parameter(Mandatory = $true)][string]$SumsPath,
        [Parameter(Mandatory = $true)][string]$AssetName
    )
    $expected = $null
    $lines = (Get-Content -Path $SumsPath -Raw) -split "`r?`n"
    foreach ($line in $lines) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        if ($line -notmatch '^\s*([0-9a-fA-F]{64})\s+\*?([^\s]+)\s*$') {
            throw "Refusing to install: malformed line in SHA256SUMS."
        }
        $hash = $Matches[1].ToLowerInvariant()
        $name = $Matches[2]
        if ($name -ceq $AssetName) {
            if ($null -ne $expected -and $expected -ne $hash) {
                throw "Refusing to install: conflicting SHA256SUMS entries for $AssetName."
            }
            $expected = $hash
        }
    }
    if (-not $expected) {
        throw "Refusing to install: $AssetName is not listed in SHA256SUMS."
    }
    return $expected
}

function Get-FileSha256Hex {
    param([Parameter(Mandatory = $true)][string]$Path)
    $sha = [Security.Cryptography.SHA256]::Create()
    $stream = [IO.File]::OpenRead($Path)
    try {
        return ([BitConverter]::ToString($sha.ComputeHash($stream))).Replace('-', '').ToLowerInvariant()
    } finally {
        $stream.Dispose()
        $sha.Dispose()
    }
}

function Install-BinariesWithRollback {
    param(
        [Parameter(Mandatory = $true)][string]$StagedExe,
        [Parameter(Mandatory = $true)][string]$DestDir,
        [Parameter(Mandatory = $true)][string[]]$Targets
    )
    New-Item -ItemType Directory -Force -Path $DestDir | Out-Null
    $replaced = @()
    try {
        foreach ($name in $Targets) {
            Copy-Item -Path $StagedExe -Destination (Join-Path $DestDir "$name.new") -Force
        }
        foreach ($name in $Targets) {
            $target = Join-Path $DestDir $name
            Get-ChildItem -Path $DestDir -Filter "$name.*.old" -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
            $old = if (Test-Path -LiteralPath $target) { "$target.$([Guid]::NewGuid().ToString('N')).old" } else { $null }
            $replaced += , @($target, $old)
            if ($old) {
                Move-Item -LiteralPath $target -Destination $old
            }
            Move-Item -LiteralPath "$target.new" -Destination $target
        }
    } catch {
        $failure = $_
        [array]::Reverse($replaced)
        foreach ($pair in $replaced) {
            $target, $old = $pair
            try {
                if (-not $old) {
                    Remove-Item -LiteralPath $target -Force -ErrorAction SilentlyContinue
                } elseif (Test-Path -LiteralPath $old) {
                    $new = "$target.$([Guid]::NewGuid().ToString('N')).old"
                    if (Test-Path -LiteralPath $target) {
                        Move-Item -LiteralPath $target -Destination $new
                    }
                    Move-Item -LiteralPath $old -Destination $target
                    Remove-Item -LiteralPath $new -Force -ErrorAction SilentlyContinue
                }
            } catch {
                Write-Warning "Failed restoring previous binary at ${target}: old copy kept at $old"
            }
        }
        throw $failure
    } finally {
        foreach ($name in $Targets) {
            Remove-Item -LiteralPath (Join-Path $DestDir "$name.new") -Force -ErrorAction SilentlyContinue
        }
    }
    foreach ($pair in $replaced) {
        if ($pair[1]) {
            Remove-Item -LiteralPath $pair[1] -Force -ErrorAction SilentlyContinue
        }
    }
}

function Add-ToUserPath {
    param([Parameter(Mandatory = $true)][string]$Dir)
    $normDir = $Dir.TrimEnd('\')
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
    try {
        $userPath = $key.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
        $entries = @($userPath -split ';' | Where-Object { $_ })
        $alreadyPresent = $entries | Where-Object { $_.TrimEnd('\') -ieq $normDir }
        if (-not $alreadyPresent) {
            $kind = if ($key.GetValueNames() -contains 'Path') { $key.GetValueKind('Path') } else { 'ExpandString' }
            $key.SetValue('Path', (($entries + $Dir) -join ';'), $kind)
            $notify = "HERMES_INSTALL_$([Guid]::NewGuid().ToString('N'))"
            [Environment]::SetEnvironmentVariable($notify, '1', 'User')
            [Environment]::SetEnvironmentVariable($notify, $null, 'User')
            $env:Path = "$Dir;$env:Path"
            Write-Output "Added $Dir to your user PATH; open a new terminal to use it elsewhere."
        }
    } finally {
        $key.Close()
    }
}

function Invoke-PostInstall {
    param(
        [Parameter(Mandatory = $true)][string]$BinPath,
        [Parameter(Mandatory = $true)][string]$Mode,
        [Parameter(Mandatory = $true)][bool]$Interactive
    )
    if ($Mode -eq 'never') {
        Write-Output 'Post-install setup skipped (-SkipSetup).'
        return
    }
    if ($Mode -eq 'auto') {
        if (-not $Interactive) {
            Write-Output 'Post-install setup skipped (non-interactive install). Run later:'
            Write-Output "  `"$BinPath`" setup"
            return
        }
        $reply = Read-Host 'Run post-install setup flow (doctor + auth status + setup)? [Y/n]'
        if (-not [string]::IsNullOrWhiteSpace($reply) -and $reply -notmatch '^(?i)y(es)?$') {
            Write-Output 'Post-install setup skipped. Run later:'
            Write-Output "  `"$BinPath`" setup"
            return
        }
    }
    $saved = @{}
    foreach ($var in @('PYTHONHOME', 'PYTHONPATH', 'PYTHONSTARTUP', 'VIRTUAL_ENV', 'CONDA_PREFIX', 'CONDA_DEFAULT_ENV', 'PIP_REQUIRE_VIRTUALENV', 'PYTHONUSERBASE')) {
        $saved[$var] = [Environment]::GetEnvironmentVariable($var, 'Process')
        [Environment]::SetEnvironmentVariable($var, $null, 'Process')
    }
    try {
        Write-Output ''
        Write-Output 'Running post-install verification...'
        & $BinPath doctor
        Write-Output ''
        Write-Output 'Current auth/platform status:'
        & $BinPath auth status
        if ($Interactive) {
            $reply = Read-Host 'Run interactive setup now? [Y/n]'
            if ([string]::IsNullOrWhiteSpace($reply) -or $reply -match '^(?i)y(es)?$') {
                & $BinPath setup
            }
        } else {
            Write-Output 'Interactive setup skipped (non-interactive shell). Run later:'
            Write-Output "  `"$BinPath`" setup"
        }
    } finally {
        foreach ($var in $saved.Keys) {
            [Environment]::SetEnvironmentVariable($var, $saved[$var], 'Process')
        }
    }
}

if ($MyInvocation.InvocationName -ne '.') {
    if ($Help) {
        Show-Usage
        return
    }
    if ($PSVersionTable.Platform -and $PSVersionTable.Platform -ne 'Win32NT') {
        throw 'Refusing to install: scripts/install.ps1 supports Windows only. Use scripts/install.sh on Linux/macOS.'
    }
    if (-not [Environment]::Is64BitOperatingSystem) {
        throw 'Refusing to install: prebuilt Windows releases require 64-bit Windows.'
    }
    $Repo = $Repo.Trim()
    if ($Repo -notmatch '^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$') {
        throw "Refusing to install: invalid repository slug '$Repo'."
    }
    $Version = $Version.Trim()
    if ($Version -ne 'latest' -and $Version -notmatch '^[A-Za-z0-9][A-Za-z0-9._+-]*$') {
        throw "Refusing to install: invalid version tag '$Version'."
    }

    $loopbackBase = $null
    if (-not [string]::IsNullOrWhiteSpace($ReleaseBaseUrl)) {
        $ReleaseBaseUrl = $ReleaseBaseUrl.Trim().TrimEnd('/')
        if (-not [Uri]::TryCreate($ReleaseBaseUrl, [UriKind]::Absolute, [ref]$loopbackBase) -or
            -not ($loopbackBase.Scheme -eq 'https' -or ($loopbackBase.Scheme -eq 'http' -and $loopbackBase.IsLoopback))) {
            throw "Refusing to install: ReleaseBaseUrl '$ReleaseBaseUrl' must use HTTPS (or loopback HTTP for tests)."
        }
    }

    $resolvedInstallDir = if (-not [string]::IsNullOrWhiteSpace($InstallDir)) {
        $InstallDir
    } elseif ($env:LOCALAPPDATA) {
        Join-Path $env:LOCALAPPDATA 'Programs\ReformedAgent'
    } else {
        Join-Path $HOME '.local\bin'
    }
    $homeRoot = if ($env:HOME) { $env:HOME } elseif ($env:USERPROFILE) { $env:USERPROFILE } else { $HOME }
    $resolvedHermesHome = if (-not [string]::IsNullOrWhiteSpace($HermesHome)) {
        $HermesHome
    } else {
        Join-Path $homeRoot '.hermes-agent-ultra'
    }
    $installLegacy = $LegacyAlias.IsPresent -or (Test-Truthy $env:INSTALL_LEGACY_ALIAS)
    $skipPathUpdate = $NoModifyPath.IsPresent -or (Test-Truthy $env:HERMES_NO_MODIFY_PATH)
    $requireSig = Test-Truthy $env:HERMES_REQUIRE_SIGNATURE
    $setupMode = if ($SkipSetup) {
        'never'
    } elseif ($Setup) {
        'always'
    } elseif ($env:RUN_SETUP_MODE) {
        $env:RUN_SETUP_MODE
    } else {
        'auto'
    }
    if ($setupMode -notin @('auto', 'always', 'never')) {
        throw "Invalid RUN_SETUP_MODE '$setupMode' (expected auto|always|never)."
    }

    $asset = 'hermes-windows-x86_64.zip'
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("hermes-install-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    $savedProtocols = [Net.ServicePointManager]::SecurityProtocol
    try {
        [Net.ServicePointManager]::SecurityProtocol = $savedProtocols -bor [Net.SecurityProtocolType]::Tls12

        $resolvedVersion = $Version
        if ($resolvedVersion -eq 'latest' -and -not $loopbackBase) {
            $resolvedVersion = Resolve-LatestTag -RepoSlug $Repo
        }
        $baseUrl = if ($loopbackBase) {
            $ReleaseBaseUrl
        } else {
            "https://github.com/$Repo/releases/download/$resolvedVersion"
        }

        $sumsPath = Join-Path $tmp 'SHA256SUMS'
        $zipPath = Join-Path $tmp $asset
        Write-Output "Installing hermes-agent-ultra from $baseUrl/$asset"
        Invoke-VerifiedDownload -Url "$baseUrl/SHA256SUMS" -OutFile $sumsPath -LoopbackBase $loopbackBase

        $cosign = Get-Command cosign -ErrorAction SilentlyContinue
        if ($cosign) {
            if ($resolvedVersion -eq 'latest') {
                throw 'Refusing to install: cannot verify cosign signature without a concrete release tag; pass -Version.'
            }
            $bundlePath = Join-Path $tmp 'SHA256SUMS.sigstore.json'
            Invoke-VerifiedDownload -Url "$baseUrl/SHA256SUMS.sigstore.json" -OutFile $bundlePath -LoopbackBase $loopbackBase
            $identityRegex = "^https://github\.com/$([regex]::Escape($Repo))/\.github/workflows/release\.yml@refs/tags/$([regex]::Escape($resolvedVersion))`$"
            $cosignOut = & $cosign.Source verify-blob `
                --bundle $bundlePath `
                --certificate-identity-regexp $identityRegex `
                --certificate-oidc-issuer 'https://token.actions.githubusercontent.com' `
                $sumsPath 2>&1
            if ($LASTEXITCODE -ne 0) {
                $cosignOut | ForEach-Object { Write-Error $_ }
                throw "Refusing to install: SHA256SUMS is not signed by $Repo's release workflow for $resolvedVersion."
            }
            Write-Output 'Verified the signature on SHA256SUMS.'
        } elseif ($requireSig) {
            throw 'Refusing to install: HERMES_REQUIRE_SIGNATURE is set but cosign is not on PATH.'
        } else {
            Write-Output 'cosign not found; checking SHA256SUMS without its signature. Install cosign to verify it.'
        }

        Invoke-VerifiedDownload -Url "$baseUrl/$asset" -OutFile $zipPath -LoopbackBase $loopbackBase
        $expectedHash = Get-ExpectedAssetHash -SumsPath $sumsPath -AssetName $asset
        $actualHash = Get-FileSha256Hex -Path $zipPath
        if ($actualHash -cne $expectedHash) {
            throw "Refusing to install: $asset does not match SHA256SUMS (expected $expectedHash, got $actualHash)."
        }
        Write-Output "Verified $asset against SHA256SUMS."

        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $stagedExe = Join-Path $tmp 'hermes-staged.exe'
        $archive = [IO.Compression.ZipFile]::OpenRead($zipPath)
        try {
            $candidates = @('hermes-agent-ultra.exe', 'hermes-ultra.exe', 'hermes.exe')
            $entry = $null
            foreach ($candidate in $candidates) {
                $match = $archive.Entries | Where-Object {
                    $_.FullName -ceq $candidate -and $_.FullName -notmatch '[/\\]' -and $_.FullName -notmatch '\.\.'
                } | Select-Object -First 1
                if ($match) {
                    $entry = $match
                    break
                }
            }
            if ($null -eq $entry) {
                throw "Refusing to install: no recognized executable found inside $asset."
            }
            $inStream = $entry.Open()
            $outStream = [IO.File]::Create($stagedExe)
            try {
                $inStream.CopyTo($outStream)
            } finally {
                $outStream.Dispose()
                $inStream.Dispose()
            }
        } finally {
            $archive.Dispose()
        }

        $versionOut = & $stagedExe --version 2>&1
        if ($LASTEXITCODE -ne 0) {
            throw "Refusing to install: extracted binary failed '--version' check."
        }

        $targets = @('hermes-agent-ultra.exe', 'hermes-ultra.exe')
        if ($installLegacy) {
            $targets += 'hermes.exe'
        } else {
            Write-Output 'Legacy hermes.exe alias not installed by default; existing upstream hermes commands are left untouched.'
        }
        Install-BinariesWithRollback -StagedExe $stagedExe -DestDir $resolvedInstallDir -Targets $targets
        $canonicalPath = Join-Path $resolvedInstallDir 'hermes-agent-ultra.exe'
        Write-Output "Installed to $canonicalPath ($versionOut)"

        New-Item -ItemType Directory -Force -Path $resolvedHermesHome | Out-Null
        $soulPath = Join-Path $resolvedHermesHome 'SOUL.md'
        if (-not (Test-Path -LiteralPath $soulPath)) {
            [IO.File]::WriteAllText($soulPath, $script:DefaultSoulMd, [Text.UTF8Encoding]::new($false))
            Write-Output "Created $soulPath (edit to customize personality)."
        }

        if ($skipPathUpdate) {
            $onPath = ($env:Path -split ';' | Where-Object { $_.TrimEnd('\') -ieq $resolvedInstallDir.TrimEnd('\') })
            if (-not $onPath) {
                Write-Output "PATH left unmodified; add $resolvedInstallDir to run hermes-ultra from any terminal."
            }
        } else {
            Add-ToUserPath -Dir $resolvedInstallDir
        }

        $interactive = [Environment]::UserInteractive -and -not [Console]::IsInputRedirected
        Invoke-PostInstall -BinPath $canonicalPath -Mode $setupMode -Interactive $interactive
    } finally {
        [Net.ServicePointManager]::SecurityProtocol = $savedProtocols
        Remove-Item -Recurse -Force -LiteralPath $tmp -ErrorAction SilentlyContinue
    }
}
