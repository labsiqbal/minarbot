[CmdletBinding()]
param(
    [string]$Distro = 'Ubuntu',
    [string]$Source,
    [string]$Token,
    [string]$Name,
    [switch]$NoOpen,
    [string]$InstallerPath,
    [string]$ArchivePath,
    [string]$ArchiveSha256,
    [string]$Tag
)
$ErrorActionPreference = 'Stop'
$origin = 'http://127.0.0.1:4317'
$installerUrl = 'https://raw.githubusercontent.com/labsiqbal/minarbot/main/scripts/install.sh'

function Get-LocalEnvironment {
    try {
        $environment = Invoke-RestMethod -Uri "$origin/api/environment" -TimeoutSec 2
        if ($environment.id -and $environment.version) { return $environment }
    } catch { }
}

function Convert-ToWslPath([string]$Path) {
    $result = & $wsl -d $Distro --exec wslpath -u $Path
    if ($LASTEXITCODE -ne 0 -or -not $result) { throw 'Could not find the installer in WSL.' }
    return $result.Trim()
}

function Quote-ProcessArgument([string]$Value) {
    return '"' + ($Value -replace '(\\*)"', '$1$1\"' -replace '(\\+)$', '$1$1') + '"'
}

if ([bool]$Source -ne [bool]$Token) { throw 'Pass both -Source and -Token to pair.' }
if ($Token -and $Token -cnotmatch '^bsi_[A-Za-z0-9_-]{43}$') { throw 'Install token is invalid.' }
if ($ArchivePath -and (-not $InstallerPath -or -not $Tag)) { throw 'Embedded archive requires its installer and release tag.' }
if ($ArchiveSha256 -and (-not $ArchivePath -or $ArchiveSha256 -cnotmatch '^[a-f0-9]{64}$')) { throw 'Embedded digest requires a bundled archive.' }
if (-not $Source -and (Get-LocalEnvironment)) {
    if (-not $NoOpen) { Start-Process $origin }
    return
}
$wsl = (Get-Command wsl.exe -ErrorAction SilentlyContinue).Source
$guide = 'Install WSL 2 in Administrator PowerShell with wsl --install -d Ubuntu. Restart if prompted, then open Ubuntu once to create your Linux user.'
if (-not $wsl) { throw $guide }
$distros = ((& $wsl --list --verbose 2>$null) -join "`n").Replace([string][char]0, '')
$match = '(?m)^\s*\*?\s*' + [regex]::Escape($Distro) + '\s+.+\s+2\s*$'
if ($LASTEXITCODE -ne 0 -or $distros -notmatch $match) { throw $guide }
$check = 'command -v python3 >/dev/null && command -v curl >/dev/null && command -v openssl >/dev/null'
& $wsl -d $Distro --exec sh -c $check
if ($LASTEXITCODE -ne 0) { throw 'In Ubuntu, run sudo apt update and sudo apt install curl python3 openssl.' }

$tempPath = $null
$child = $null
$ready = $false
try {
    if (-not $InstallerPath) {
        $tempPath = Join-Path ([IO.Path]::GetTempPath()) ("minarbot-install-" + [guid]::NewGuid() + ".sh")
        Invoke-WebRequest -Uri $installerUrl -UseBasicParsing -OutFile $tempPath
        $InstallerPath = $tempPath
    }
    $wslArguments = @('-d', $Distro, '--exec', 'sh', (Convert-ToWslPath $InstallerPath), '--wsl', '--no-open')
    if ($ArchivePath) { $wslArguments += @('--archive', (Convert-ToWslPath $ArchivePath), '--tag', $Tag) }
    if ($ArchiveSha256) { $wslArguments += @('--archive-sha256', $ArchiveSha256) }
    if ($Source) { $wslArguments += @('--source', $Source, '--token', $Token) }
    if ($Name) { $wslArguments += @('--name', $Name) }
    $quoted = ($wslArguments | ForEach-Object { Quote-ProcessArgument $_ }) -join ' '
    $child = Start-Process -FilePath $wsl -ArgumentList $quoted -PassThru -WindowStyle Hidden
    $deadline = [DateTime]::UtcNow.AddMinutes(3)
    while ([DateTime]::UtcNow -lt $deadline) {
        if (Get-LocalEnvironment) {
            if (-not $Source) { $ready = $true; break }
            try {
                $machines = (Invoke-RestMethod -Uri "$origin/api/machines" -TimeoutSec 2).machines
                if ($machines | Where-Object { $_.origin -eq $Source }) { $ready = $true; break }
            } catch { }
        }
        if ($child.HasExited) { throw 'Minarbot setup in WSL failed. Check Ubuntu network access, OpenSSL 3, and the pairing token if used.' }
        Start-Sleep -Milliseconds 500
    }
    if (-not $ready) { throw 'Minarbot did not become healthy in WSL. Check WSL localhost forwarding and port 4317.' }
    if (-not $NoOpen) { Start-Process $origin }
} finally {
    if (-not $ready -and $child -and -not $child.HasExited) { $child.Kill() }
    if ($tempPath) { Remove-Item -LiteralPath $tempPath -Force -ErrorAction SilentlyContinue }
}
