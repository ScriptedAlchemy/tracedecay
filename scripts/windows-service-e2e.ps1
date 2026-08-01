[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$TraceDecayExe
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false

if ($env:OS -ne "Windows_NT") {
    throw "windows-service-e2e.ps1 requires Windows"
}
if ([string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) {
    throw "RUNNER_TEMP must identify the isolated CI scratch directory"
}

$script:traceDecayExePath = (Resolve-Path -LiteralPath $TraceDecayExe).ProviderPath
$script:userSid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$script:taskName = "TraceDecay Daemon ($($script:userSid))"
$script:taskPath = "\$($script:taskName)"
$script:scheduler = New-Object -ComObject "Schedule.Service"
$script:scheduler.Connect()
$script:taskRoot = $script:scheduler.GetFolder("\")
$dataDir = [System.IO.Directory]::CreateDirectory(
    (Join-Path $env:RUNNER_TEMP ("tracedecay-service-e2e-" + [guid]::NewGuid().ToString("N")))
).FullName
$previousDataDir = [Environment]::GetEnvironmentVariable("TRACEDECAY_DATA_DIR", "Process")
$previousPath = $env:PATH
$createdByRun = $false
$primaryError = $null
$cleanupError = $null

function Assert-Equal {
    param(
        [AllowNull()]$Actual,
        [AllowNull()]$Expected,
        [Parameter(Mandatory = $true)][string]$Description
    )

    if ($Actual -ne $Expected) {
        throw "$Description mismatch: expected '$Expected', got '$Actual'"
    }
}

function Get-TaskOrNull {
    try {
        return $script:taskRoot.GetTask($script:taskPath)
    }
    catch {
        $exception = $_.Exception
        while ($null -ne $exception) {
            $hresult = "{0:X8}" -f ($exception.HResult -band 0xffffffffL)
            if ($hresult -in @("80070002", "80070003")) {
                return $null
            }
            $exception = $exception.InnerException
        }
        throw
    }
}

function Get-TaskLifecycleState {
    $task = Get-TaskOrNull
    if ($null -eq $task) {
        return "Missing"
    }

    $schedulerState = [int]$task.State
    $enabled = [bool]$task.Enabled
    if ($schedulerState -in @(2, 4)) {
        if ($enabled) {
            return "RunningEnabled"
        }
        return "RunningDisabled"
    }
    if ($schedulerState -eq 3 -and $enabled) {
        return "StoppedEnabled"
    }
    if ($schedulerState -eq 1 -and -not $enabled) {
        return "StoppedDisabled"
    }
    throw "Task Scheduler returned inconsistent state=$schedulerState enabled=$enabled"
}

function Invoke-TraceDecayRaw {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$Arguments,
        [switch]$Quiet
    )

    if (-not $Quiet) {
        Write-Host "tracedecay $($Arguments -join ' ')"
    }
    $lines = @(
        & $script:traceDecayExePath @Arguments 2>&1 |
            ForEach-Object { $_.ToString() }
    )
    $exitCode = $LASTEXITCODE
    return [pscustomobject]@{
        ExitCode = $exitCode
        Output = $lines -join [Environment]::NewLine
    }
}

function Write-CommandOutput {
    param([Parameter(Mandatory = $true)]$Result)

    if (-not [string]::IsNullOrWhiteSpace($Result.Output)) {
        Write-Host $Result.Output
    }
}

function Assert-CommandSucceeded {
    param(
        [Parameter(Mandatory = $true)]$Result,
        [Parameter(Mandatory = $true)][string]$Description
    )

    Write-CommandOutput -Result $Result
    if ($Result.ExitCode -ne 0) {
        throw "$Description exited $($Result.ExitCode): $($Result.Output)"
    }
}

function Invoke-TraceDecay {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$Arguments
    )

    $result = Invoke-TraceDecayRaw -Arguments $Arguments
    Assert-CommandSucceeded -Result $result -Description "tracedecay $($Arguments -join ' ')"
    return $result.Output
}

function Assert-TaskSddl {
    param([Parameter(Mandatory = $true)]$Task)

    $sddl = [string]$Task.GetSecurityDescriptor(0x00000001 -bor 0x00000004)
    $descriptor = [System.Security.AccessControl.RawSecurityDescriptor]::new($sddl)
    Assert-Equal -Actual $descriptor.Owner.Value -Expected $script:userSid `
        -Description "task SDDL owner"

    $protectedFlag = [System.Security.AccessControl.ControlFlags]::DiscretionaryAclProtected
    if (($descriptor.ControlFlags -band $protectedFlag) -eq 0) {
        throw "task SDDL DACL is not protected: $sddl"
    }

    $dacl = $descriptor.DiscretionaryAcl
    if ($null -eq $dacl -or $dacl.Count -ne 2) {
        throw "task SDDL must contain exactly two ACEs: $sddl"
    }

    $aceSids = @()
    for ($index = 0; $index -lt $dacl.Count; $index++) {
        $ace = $dacl[$index]
        Assert-Equal -Actual $ace.AceType `
            -Expected ([System.Security.AccessControl.AceType]::AccessAllowed) `
            -Description "task SDDL ACE type"
        Assert-Equal -Actual $ace.AceFlags `
            -Expected ([System.Security.AccessControl.AceFlags]::None) `
            -Description "task SDDL ACE flags"
        Assert-Equal -Actual $ace.AccessMask -Expected 0x10000000 `
            -Description "task SDDL ACE access mask"
        $aceSids += $ace.SecurityIdentifier.Value
    }
    if ($aceSids -notcontains "S-1-5-18" -or $aceSids -notcontains $script:userSid) {
        throw "task SDDL must grant only LocalSystem and $($script:userSid): $sddl"
    }
}

function Assert-TaskDefinition {
    $task = Get-TaskOrNull
    if ($null -eq $task) {
        throw "scheduled task '$($script:taskPath)' is missing"
    }

    $definition = $task.Definition
    $principal = $definition.Principal
    Assert-Equal -Actual $principal.Id -Expected "Author" `
        -Description "task principal id"
    Assert-Equal -Actual $principal.UserId -Expected $script:userSid `
        -Description "task principal SID"
    Assert-Equal -Actual ([int]$principal.LogonType) -Expected 3 `
        -Description "task principal logon type"
    Assert-Equal -Actual ([int]$principal.RunLevel) -Expected 0 `
        -Description "task principal run level"

    $actions = $definition.Actions
    Assert-Equal -Actual $actions.Count -Expected 1 -Description "task action count"
    $action = $actions.Item(1)
    Assert-Equal -Actual ([int]$action.Type) -Expected 0 `
        -Description "task action type"
    Assert-Equal -Actual $action.Path -Expected $script:traceDecayExePath `
        -Description "task action path"
    $expectedArguments = 'daemon run --profile-root "{0}"' -f $dataDir
    Assert-Equal -Actual $action.Arguments -Expected $expectedArguments `
        -Description "task action arguments"
    if (-not [string]::IsNullOrWhiteSpace([string]$action.WorkingDirectory)) {
        throw "task action working directory must be empty, got '$($action.WorkingDirectory)'"
    }

    $triggers = $definition.Triggers
    Assert-Equal -Actual $triggers.Count -Expected 1 -Description "task trigger count"
    $trigger = $triggers.Item(1)
    Assert-Equal -Actual ([int]$trigger.Type) -Expected 9 `
        -Description "task trigger type"
    Assert-Equal -Actual $trigger.UserId -Expected $script:userSid `
        -Description "task trigger SID"
    Assert-TaskSddl -Task $task
}

function Test-TaskOwnedByRun {
    try {
        Assert-TaskDefinition
        return $true
    }
    catch {
        return $false
    }
}

function Assert-ServiceObservation {
    param(
        [Parameter(Mandatory = $true)][string]$ExpectedState,
        [Parameter(Mandatory = $true)][bool]$Connectable
    )

    $taskState = Get-TaskLifecycleState
    Assert-Equal -Actual $taskState -Expected $ExpectedState `
        -Description "Task Scheduler lifecycle state"

    $statusResult = Invoke-TraceDecayRaw -Arguments @("daemon", "status") -Quiet
    if ($statusResult.ExitCode -ne 0) {
        throw "tracedecay daemon status exited $($statusResult.ExitCode): $($statusResult.Output)"
    }
    $status = $statusResult.Output
    $statePattern = "(?m)^state: {0}\r?$" -f [regex]::Escape($ExpectedState)
    if ($status -notmatch $statePattern) {
        throw "daemon status did not report state $ExpectedState`: $status"
    }
    $endpoint = [regex]::Match(
        $status,
        "(?m)^endpoint: .+ \((?<state>[^)]+)\)\r?$"
    )
    if (-not $endpoint.Success) {
        throw "daemon status omitted endpoint connectivity: $status"
    }
    $endpointState = $endpoint.Groups["state"].Value
    if ($Connectable -and $endpointState -ne "connectable") {
        throw "daemon endpoint is '$endpointState', expected 'connectable'"
    }
    if (-not $Connectable -and $endpointState -eq "connectable") {
        throw "daemon endpoint is connectable, expected nonconnectable"
    }
}

function Wait-ServiceObservation {
    param(
        [Parameter(Mandatory = $true)][string]$ExpectedState,
        [Parameter(Mandatory = $true)][bool]$Connectable,
        [Parameter(Mandatory = $true)][DateTime]$Deadline
    )

    $lastObservation = "deadline elapsed before first observation"
    while ([DateTime]::UtcNow -lt $Deadline) {
        try {
            Assert-ServiceObservation -ExpectedState $ExpectedState -Connectable $Connectable
            return
        }
        catch {
            $lastObservation = $_.Exception.Message
        }
        $remaining = $Deadline - [DateTime]::UtcNow
        if ($remaining -le [TimeSpan]::Zero) {
            break
        }
        Start-Sleep -Milliseconds ([Math]::Min(500, [Math]::Max(1, $remaining.TotalMilliseconds)))
    }

    throw "timed out waiting for $ExpectedState/connectable=$Connectable`: $lastObservation"
}

try {
    $env:TRACEDECAY_DATA_DIR = $dataDir
    $env:PATH = "$(Split-Path -Parent $script:traceDecayExePath);$previousPath"

    if ($null -ne (Get-TaskOrNull)) {
        throw "refusing to run: scheduled task '$($script:taskPath)' already exists"
    }

    $install = Invoke-TraceDecayRaw -Arguments @(
        "daemon",
        "install-service",
        "--no-start"
    )
    if ($null -ne (Get-TaskOrNull)) {
        $createdByRun = $true
    }
    Assert-CommandSucceeded -Result $install -Description "daemon install-service --no-start"
    if (-not $createdByRun) {
        throw "daemon install-service succeeded without creating '$($script:taskPath)'"
    }

    Assert-TaskDefinition
    Wait-ServiceObservation -ExpectedState "StoppedDisabled" -Connectable $false `
        -Deadline ([DateTime]::UtcNow.AddSeconds(10))

    $startDeadline = [DateTime]::UtcNow.AddMinutes(3)
    Invoke-TraceDecay -Arguments @("daemon", "start") | Out-Null
    Wait-ServiceObservation -ExpectedState "RunningDisabled" -Connectable $true `
        -Deadline $startDeadline

    Invoke-TraceDecay -Arguments @("daemon", "stop") | Out-Null
    Wait-ServiceObservation -ExpectedState "StoppedDisabled" -Connectable $false `
        -Deadline ([DateTime]::UtcNow.AddMinutes(1))

    Invoke-TraceDecay -Arguments @("daemon", "uninstall-service") | Out-Null
    Wait-ServiceObservation -ExpectedState "Missing" -Connectable $false `
        -Deadline ([DateTime]::UtcNow.AddSeconds(10))
    $createdByRun = $false

    if ($null -ne (Get-TaskOrNull)) {
        throw "refusing to reinstall: scheduled task '$($script:taskPath)' appeared unexpectedly"
    }
    $installDeadline = [DateTime]::UtcNow.AddMinutes(3)
    $install = Invoke-TraceDecayRaw -Arguments @("daemon", "install-service")
    if ($null -ne (Get-TaskOrNull)) {
        $createdByRun = $true
    }
    Assert-CommandSucceeded -Result $install -Description "daemon install-service"
    if (-not $createdByRun) {
        throw "daemon install-service succeeded without creating '$($script:taskPath)'"
    }

    Assert-TaskDefinition
    Wait-ServiceObservation -ExpectedState "RunningEnabled" -Connectable $true `
        -Deadline $installDeadline

    Invoke-TraceDecay -Arguments @("daemon", "stop") | Out-Null
    Wait-ServiceObservation -ExpectedState "StoppedEnabled" -Connectable $false `
        -Deadline ([DateTime]::UtcNow.AddMinutes(1))

    $startDeadline = [DateTime]::UtcNow.AddMinutes(3)
    Invoke-TraceDecay -Arguments @("daemon", "start") | Out-Null
    Wait-ServiceObservation -ExpectedState "RunningEnabled" -Connectable $true `
        -Deadline $startDeadline

    Invoke-TraceDecay -Arguments @("daemon", "uninstall-service") | Out-Null
    Wait-ServiceObservation -ExpectedState "Missing" -Connectable $false `
        -Deadline ([DateTime]::UtcNow.AddSeconds(10))
    $createdByRun = $false
    Write-Host "Windows native service lifecycle E2E passed"
}
catch {
    $primaryError = $_
}
finally {
    $taskProvenRemoved = $false
    if ($createdByRun) {
        try {
            $task = Get-TaskOrNull
            if ($null -ne $task) {
                if (-not (Test-TaskOwnedByRun)) {
                    throw "refusing cleanup: '$($script:taskPath)' is not owned by this run"
                }
                $cleanup = Invoke-TraceDecayRaw -Arguments @(
                    "daemon",
                    "uninstall-service"
                )
                Write-CommandOutput -Result $cleanup
                $task = Get-TaskOrNull
                if ($null -ne $task) {
                    try {
                        $task.Stop(0)
                    }
                    catch {
                        Write-Warning "fallback task stop failed: $($_.Exception.Message)"
                    }
                    $script:taskRoot.DeleteTask($script:taskName, 0)
                }
            }
            if ($null -ne (Get-TaskOrNull)) {
                throw "cleanup left scheduled task '$($script:taskPath)' registered"
            }
            $taskProvenRemoved = $true
        }
        catch {
            $cleanupError = $_
        }
    }
    elseif ($null -eq (Get-TaskOrNull)) {
        $taskProvenRemoved = $true
    }

    [Environment]::SetEnvironmentVariable("TRACEDECAY_DATA_DIR", $previousDataDir, "Process")
    $env:PATH = $previousPath
    if ($null -ne $script:taskRoot) {
        [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($script:taskRoot)
    }
    if ($null -ne $script:scheduler) {
        [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($script:scheduler)
    }
    if ($taskProvenRemoved -and $null -eq $primaryError -and $null -eq $cleanupError) {
        Remove-Item -LiteralPath $dataDir -Recurse -Force
    }
    else {
        Write-Warning "retained service E2E profile for diagnosis: $dataDir"
    }
}

if ($null -ne $primaryError) {
    if ($null -ne $cleanupError) {
        throw "service E2E failed: $($primaryError.Exception.Message); cleanup failed: $($cleanupError.Exception.Message)"
    }
    throw $primaryError
}
if ($null -ne $cleanupError) {
    throw $cleanupError
}
