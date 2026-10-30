param([switch]$RedirectTemp)

$ErrorActionPreference = 'Stop'
if ($env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    throw 'Windows CI tuning is restricted to disposable GitHub-hosted runners.'
}

$service = Get-Service -Name WinDefend
if ($service.Status -ne 'Stopped') {
    $defender = Get-MpComputerStatus
    if ($defender.RealTimeProtectionEnabled) {
        Set-MpPreference -DisableRealtimeMonitoring $true
        $defender = Get-MpComputerStatus
    }
    if ($null -eq $defender.RealTimeProtectionEnabled -or $defender.RealTimeProtectionEnabled) {
        throw 'Defender real-time scanning is still enabled or could not be verified. Runner tuning did not take effect.'
    }
}
Write-Host 'Verified: Defender real-time scanning is disabled on this disposable runner.'

if ($RedirectTemp) {
    $fastTemp = Join-Path $env:RUNNER_TEMP 'tmp'
    New-Item -ItemType Directory -Path $fastTemp -Force | Out-Null
    "TMP=$fastTemp" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append
    "TEMP=$fastTemp" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append
}

$os = Get-CimInstance Win32_OperatingSystem
$drive = [System.IO.DriveInfo]::new($env:RUNNER_TEMP)
@"
### Windows build preflight
- Defender real-time scanning: verified off
- Logical CPUs: $env:NUMBER_OF_PROCESSORS
- Available RAM: $([math]::Round($os.FreePhysicalMemory / 1MB, 1)) GiB
- Workspace drive free space: $([math]::Round($drive.AvailableFreeSpace / 1GB, 1)) GiB
- Temporary files redirected: $RedirectTemp
"@ | Out-File -FilePath $env:GITHUB_STEP_SUMMARY -Encoding utf8 -Append
