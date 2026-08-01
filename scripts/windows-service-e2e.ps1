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

$script:traceDecaySourcePath = (Resolve-Path -LiteralPath $TraceDecayExe).ProviderPath
$script:traceDecayExePath = $script:traceDecaySourcePath
$script:expectedTaskExecutablePath = $script:traceDecayExePath
$script:userSid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$script:taskName = "TraceDecay Daemon ($($script:userSid))"
$script:taskPath = "\$($script:taskName)"
$script:scheduler = New-Object -ComObject "Schedule.Service"
$script:scheduler.Connect()
$script:taskRoot = $script:scheduler.GetFolder("\")
$testRoot = [System.IO.Directory]::CreateDirectory(
    (Join-Path $env:RUNNER_TEMP ("tracedecay-service-e2e-" + [guid]::NewGuid().ToString("N")))
).FullName
$dataDir = [System.IO.Directory]::CreateDirectory(
    (Join-Path $testRoot "native-profile")
).FullName
$previousDataDir = [Environment]::GetEnvironmentVariable("TRACEDECAY_DATA_DIR", "Process")
$previousPath = $env:PATH
$createdByRun = $false
$script:cleanupTaskNames = @()
$script:packageContexts = @()
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

function Get-TaskAtPathOrNull {
    param([Parameter(Mandatory = $true)][string]$TaskPath)

    try {
        return $script:taskRoot.GetTask($TaskPath)
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

function Get-TaskOrNull {
    return Get-TaskAtPathOrNull -TaskPath $script:taskPath
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
    Assert-Equal -Actual $action.Path -Expected $script:expectedTaskExecutablePath `
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

function Assert-PathEqual {
    param(
        [Parameter(Mandatory = $true)][string]$Actual,
        [Parameter(Mandatory = $true)][string]$Expected,
        [Parameter(Mandatory = $true)][string]$Description
    )

    $actualPath = [System.IO.Path]::GetFullPath($Actual)
    $expectedPath = [System.IO.Path]::GetFullPath($Expected)
    if (-not [string]::Equals(
        $actualPath,
        $expectedPath,
        [System.StringComparison]::OrdinalIgnoreCase
    )) {
        throw "$Description mismatch: expected '$expectedPath', got '$actualPath'"
    }
}

function Get-Sha256 {
    param([Parameter(Mandatory = $true)][string]$Path)

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "cannot hash missing file '$Path'"
    }
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
}

function New-ScoopPackageContext {
    param(
        [Parameter(Mandatory = $true)][string]$PackageId,
        [Parameter(Mandatory = $true)][string]$TaskPrefix
    )

    $packageRoot = Join-Path $testRoot "scoop\apps\$PackageId"
    $oldDirectory = [System.IO.Directory]::CreateDirectory(
        (Join-Path $packageRoot "1.0.0-e2e")
    ).FullName
    $newDirectory = [System.IO.Directory]::CreateDirectory(
        (Join-Path $packageRoot "1.0.1-e2e")
    ).FullName
    $oldExecutable = Join-Path $oldDirectory "tracedecay.exe"
    $newExecutable = Join-Path $newDirectory "tracedecay.exe"
    Copy-Item -LiteralPath $script:traceDecaySourcePath -Destination $oldExecutable
    Copy-Item -LiteralPath $script:traceDecaySourcePath -Destination $newExecutable
    Assert-Equal -Actual (Get-Sha256 -Path $oldExecutable) `
        -Expected (Get-Sha256 -Path $script:traceDecaySourcePath) `
        -Description "$PackageId staged old binary SHA-256"
    Assert-Equal -Actual (Get-Sha256 -Path $newExecutable) `
        -Expected (Get-Sha256 -Path $script:traceDecaySourcePath) `
        -Description "$PackageId staged new binary SHA-256"

    $runtimeDirectory = Join-Path $env:LOCALAPPDATA "TraceDecay\service\$PackageId"
    return [pscustomobject]@{
        PackageId = $PackageId
        TaskName = "$TaskPrefix ($($script:userSid))"
        TaskPath = "\$TaskPrefix ($($script:userSid))"
        OldExecutable = $oldExecutable
        NewExecutable = $newExecutable
        RuntimeDirectory = $runtimeDirectory
        RuntimeExecutable = Join-Path $runtimeDirectory "tracedecay.exe"
        StateFile = Join-Path $runtimeDirectory "scoop-state.json"
        RetainedStateFile = Join-Path $runtimeDirectory "scoop-state.retained-e2e.json"
        ProfileRoot = [System.IO.Directory]::CreateDirectory(
            (Join-Path $testRoot "$PackageId-profile")
        ).FullName
    }
}

function Set-ScoopPackageContext {
    param(
        [Parameter(Mandatory = $true)]$Context,
        [Parameter(Mandatory = $true)]
        [ValidateSet("Old", "New")]
        [string]$Version
    )

    $executable = if ($Version -eq "Old") {
        $Context.OldExecutable
    }
    else {
        $Context.NewExecutable
    }
    $script:traceDecayExePath = $executable
    $script:expectedTaskExecutablePath = $Context.RuntimeExecutable
    $script:taskName = $Context.TaskName
    $script:taskPath = $Context.TaskPath
    $script:dataDir = $Context.ProfileRoot
    $env:TRACEDECAY_DATA_DIR = $Context.ProfileRoot
    $env:PATH = "$(Split-Path -Parent $executable);$previousPath"
}

function Get-ScoopPackageSnapshot {
    param([Parameter(Mandatory = $true)]$Context)

    Set-ScoopPackageContext -Context $Context -Version New
    $task = Get-TaskOrNull
    if ($null -eq $task) {
        throw "scheduled task '$($Context.TaskPath)' is missing"
    }
    $markerExists = Test-Path -LiteralPath $Context.StateFile -PathType Leaf
    $runtimeExists = Test-Path -LiteralPath $Context.RuntimeExecutable -PathType Leaf
    return [pscustomobject]@{
        Xml = [string]$task.Xml
        Sddl = [string]$task.GetSecurityDescriptor(0x00000001 -bor 0x00000004)
        SchedulerState = [int]$task.State
        Enabled = [bool]$task.Enabled
        MarkerExists = $markerExists
        MarkerHash = if ($markerExists) { Get-Sha256 -Path $Context.StateFile } else { $null }
        RuntimeExists = $runtimeExists
        RuntimeHash = if ($runtimeExists) {
            Get-Sha256 -Path $Context.RuntimeExecutable
        }
        else {
            $null
        }
    }
}

function Assert-ScoopPackageSnapshot {
    param(
        [Parameter(Mandatory = $true)]$Context,
        [Parameter(Mandatory = $true)]$Expected,
        [string]$ExpectedState,
        [bool]$Connectable,
        [switch]$SkipObservation
    )

    $actual = Get-ScoopPackageSnapshot -Context $Context
    foreach ($property in @(
        "Xml",
        "Sddl",
        "SchedulerState",
        "Enabled",
        "MarkerExists",
        "MarkerHash",
        "RuntimeExists",
        "RuntimeHash"
    )) {
        Assert-Equal -Actual $actual.$property -Expected $Expected.$property `
            -Description "$($Context.PackageId) sibling $property"
    }
    if (-not $SkipObservation) {
        Assert-ServiceObservation -ExpectedState $ExpectedState -Connectable $Connectable
    }
}

function Assert-ScoopMarker {
    param(
        [Parameter(Mandatory = $true)]$Context,
        [Parameter(Mandatory = $true)]$Snapshot,
        [Parameter(Mandatory = $true)][bool]$Enabled,
        [Parameter(Mandatory = $true)][bool]$Running
    )

    if (-not (Test-Path -LiteralPath $Context.StateFile -PathType Leaf)) {
        throw "$($Context.PackageId) prepare omitted '$($Context.StateFile)'"
    }
    $marker = Get-Content -LiteralPath $Context.StateFile -Raw | ConvertFrom-Json
    Assert-Equal -Actual $marker.schema `
        -Expected "tracedecay.scoop-service-state.v1" `
        -Description "$($Context.PackageId) marker schema"
    Assert-Equal -Actual $marker.package_id -Expected $Context.PackageId `
        -Description "$($Context.PackageId) marker package"
    Assert-Equal -Actual $marker.user_sid -Expected $script:userSid `
        -Description "$($Context.PackageId) marker SID"
    Assert-Equal -Actual $marker.task_name -Expected $Context.TaskName `
        -Description "$($Context.PackageId) marker task name"
    Assert-Equal -Actual $marker.task_path -Expected $Context.TaskPath `
        -Description "$($Context.PackageId) marker task path"
    Assert-Equal -Actual $marker.task_xml -Expected $Snapshot.Xml `
        -Description "$($Context.PackageId) marker task XML"
    Assert-Equal -Actual $marker.task_sddl -Expected $Snapshot.Sddl `
        -Description "$($Context.PackageId) marker task SDDL"
    Assert-PathEqual -Actual $marker.action.executable `
        -Expected $Context.RuntimeExecutable `
        -Description "$($Context.PackageId) marker action executable"
    $expectedArguments = 'daemon run --profile-root "{0}"' -f $Context.ProfileRoot
    Assert-Equal -Actual $marker.action.arguments -Expected $expectedArguments `
        -Description "$($Context.PackageId) marker action arguments"
    Assert-PathEqual -Actual $marker.profile_root -Expected $Context.ProfileRoot `
        -Description "$($Context.PackageId) marker profile"
    Assert-Equal -Actual ([bool]$marker.enabled) -Expected $Enabled `
        -Description "$($Context.PackageId) marker enabled"
    Assert-Equal -Actual ([bool]$marker.running) -Expected $Running `
        -Description "$($Context.PackageId) marker running"
}

function Install-ScoopPackageService {
    param(
        [Parameter(Mandatory = $true)]$Context,
        [Parameter(Mandatory = $true)][string]$ExpectedState,
        [Parameter(Mandatory = $true)][bool]$Connectable,
        [switch]$NoStart
    )

    Set-ScoopPackageContext -Context $Context -Version Old
    $arguments = @("daemon", "install-service")
    if ($NoStart) {
        $arguments += "--no-start"
    }
    Assert-CommandSucceeded -Result (Invoke-TraceDecayRaw -Arguments $arguments) `
        -Description "$($Context.PackageId) install-service"
    Assert-TaskDefinition
    Wait-ServiceObservation -ExpectedState $ExpectedState -Connectable $Connectable `
        -Deadline ([DateTime]::UtcNow.AddMinutes(3))
    Assert-Equal -Actual (Get-Sha256 -Path $Context.RuntimeExecutable) `
        -Expected (Get-Sha256 -Path $Context.OldExecutable) `
        -Description "$($Context.PackageId) installed runtime SHA-256"
    if (Test-Path -LiteralPath $Context.StateFile) {
        throw "$($Context.PackageId) install unexpectedly created a Scoop marker"
    }
}

function Invoke-ScoopPrepare {
    param(
        [Parameter(Mandatory = $true)]$Context,
        [Parameter(Mandatory = $true)]$Snapshot,
        [Parameter(Mandatory = $true)][bool]$Enabled,
        [Parameter(Mandatory = $true)][bool]$Running,
        [Parameter(Mandatory = $true)]
        [ValidateSet("Old", "New")]
        [string]$Version
    )

    Set-ScoopPackageContext -Context $Context -Version $Version
    $result = Invoke-TraceDecayRaw -Arguments @(
        "package-hook",
        "scoop",
        "prepare",
        "--package-id",
        $Context.PackageId,
        "--state-file",
        $Context.StateFile
    )
    Assert-CommandSucceeded -Result $result `
        -Description "$($Context.PackageId) Scoop prepare"
    if ($null -ne (Get-TaskOrNull)) {
        throw "$($Context.PackageId) prepare left '$($Context.TaskPath)' registered"
    }
    if (Test-Path -LiteralPath $Context.RuntimeExecutable) {
        throw "$($Context.PackageId) prepare left its runtime executable"
    }
    Assert-ScoopMarker -Context $Context -Snapshot $Snapshot `
        -Enabled $Enabled -Running $Running
    Assert-ServiceObservation -ExpectedState "Missing" -Connectable $false
}

function Invoke-ScoopRestore {
    param(
        [Parameter(Mandatory = $true)]$Context,
        [Parameter(Mandatory = $true)]$Snapshot,
        [Parameter(Mandatory = $true)][string]$ExpectedState,
        [Parameter(Mandatory = $true)][bool]$Connectable
    )

    Set-ScoopPackageContext -Context $Context -Version New
    $result = Invoke-TraceDecayRaw -Arguments @(
        "package-hook",
        "scoop",
        "restore",
        "--package-id",
        $Context.PackageId,
        "--state-file",
        $Context.StateFile
    )
    Assert-CommandSucceeded -Result $result `
        -Description "$($Context.PackageId) Scoop restore"
    Assert-ServiceObservation -ExpectedState $ExpectedState -Connectable $Connectable
    Assert-TaskDefinition
    $restored = Get-ScoopPackageSnapshot -Context $Context
    Assert-Equal -Actual $restored.Xml -Expected $Snapshot.Xml `
        -Description "$($Context.PackageId) restored task XML"
    Assert-Equal -Actual $restored.Sddl -Expected $Snapshot.Sddl `
        -Description "$($Context.PackageId) restored task SDDL"
    if (Test-Path -LiteralPath $Context.StateFile) {
        throw "$($Context.PackageId) restore retained its state marker"
    }
    Assert-Equal -Actual (Get-Sha256 -Path $Context.RuntimeExecutable) `
        -Expected (Get-Sha256 -Path $Context.NewExecutable) `
        -Description "$($Context.PackageId) restored runtime SHA-256"
}

function Register-ForeignScoopTask {
    param(
        [Parameter(Mandatory = $true)]$Context,
        [Parameter(Mandatory = $true)]$Snapshot,
        [Parameter(Mandatory = $true)][string]$WrongExecutable
    )

    $document = [System.Xml.XmlDocument]::new()
    $document.PreserveWhitespace = $true
    $document.LoadXml($Snapshot.Xml)
    $namespaces = [System.Xml.XmlNamespaceManager]::new($document.NameTable)
    $namespaces.AddNamespace("task", $document.DocumentElement.NamespaceURI)
    $command = $document.SelectSingleNode(
        "//task:Actions/task:Exec/task:Command",
        $namespaces
    )
    if ($null -eq $command) {
        throw "could not locate Scoop task action in retained XML"
    }
    $command.InnerText = $WrongExecutable
    $registered = $script:taskRoot.RegisterTask(
        $Context.TaskPath,
        $document.OuterXml,
        22,
        $script:userSid,
        $null,
        3,
        $Snapshot.Sddl
    )
    if ($null -ne $registered) {
        [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($registered)
    }
}

function Remove-ScoopUninstallMarker {
    param([Parameter(Mandatory = $true)]$Context)

    if (-not (Test-Path -LiteralPath $Context.StateFile -PathType Leaf)) {
        throw "$($Context.PackageId) uninstall marker is missing"
    }
    Remove-Item -LiteralPath $Context.StateFile -Force
    if (Test-Path -LiteralPath $Context.StateFile) {
        throw "$($Context.PackageId) uninstall marker cleanup failed"
    }
}

function Assert-ScoopPackageAbsent {
    param([Parameter(Mandatory = $true)]$Context)

    Set-ScoopPackageContext -Context $Context -Version New
    if ($null -ne (Get-TaskOrNull)) {
        throw "$($Context.PackageId) task unexpectedly exists"
    }
    foreach ($path in @(
        $Context.RuntimeExecutable,
        $Context.StateFile,
        $Context.RetainedStateFile
    )) {
        if (Test-Path -LiteralPath $path) {
            throw "$($Context.PackageId) artifact unexpectedly exists: '$path'"
        }
    }
    Assert-ServiceObservation -ExpectedState "Missing" -Connectable $false
}

try {
    if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
        throw "LOCALAPPDATA is required for Scoop service E2E coverage"
    }

    $stablePackage = New-ScoopPackageContext `
        -PackageId "tracedecay" `
        -TaskPrefix "TraceDecay Daemon"
    $betaPackage = New-ScoopPackageContext `
        -PackageId "tracedecay-beta" `
        -TaskPrefix "TraceDecay Beta Daemon"
    $script:packageContexts = @($stablePackage, $betaPackage)
    foreach ($context in $script:packageContexts) {
        if ($null -ne (Get-TaskAtPathOrNull -TaskPath $context.TaskPath)) {
            throw "refusing to run: scheduled task '$($context.TaskPath)' already exists"
        }
        foreach ($path in @(
            $context.RuntimeExecutable,
            $context.StateFile,
            $context.RetainedStateFile
        )) {
            if (Test-Path -LiteralPath $path) {
                throw "refusing to overwrite existing Scoop service artifact '$path'"
            }
        }
        if (
            (Test-Path -LiteralPath $context.RuntimeDirectory -PathType Container) -and
            @(
                Get-ChildItem -LiteralPath $context.RuntimeDirectory -Force
            ).Count -ne 0
        ) {
            throw "refusing to use nonempty Scoop service runtime '$($context.RuntimeDirectory)'"
        }
    }
    $script:cleanupTaskNames = @(
        $stablePackage.TaskName,
        $betaPackage.TaskName
    )

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

    Install-ScoopPackageService `
        -Context $stablePackage `
        -ExpectedState "RunningEnabled" `
        -Connectable $true
    Install-ScoopPackageService `
        -Context $betaPackage `
        -ExpectedState "StoppedDisabled" `
        -Connectable $false `
        -NoStart

    $stableSnapshot = Get-ScoopPackageSnapshot -Context $stablePackage
    $betaSibling = Get-ScoopPackageSnapshot -Context $betaPackage
    Invoke-ScoopPrepare `
        -Context $stablePackage `
        -Snapshot $stableSnapshot `
        -Enabled $true `
        -Running $true `
        -Version Old
    Assert-ScoopPackageSnapshot `
        -Context $betaPackage `
        -Expected $betaSibling `
        -ExpectedState "StoppedDisabled" `
        -Connectable $false
    Invoke-ScoopRestore `
        -Context $stablePackage `
        -Snapshot $stableSnapshot `
        -ExpectedState "RunningEnabled" `
        -Connectable $true
    Assert-ScoopPackageSnapshot `
        -Context $betaPackage `
        -Expected $betaSibling `
        -ExpectedState "StoppedDisabled" `
        -Connectable $false

    $stableSibling = Get-ScoopPackageSnapshot -Context $stablePackage
    $betaSnapshot = Get-ScoopPackageSnapshot -Context $betaPackage
    Invoke-ScoopPrepare `
        -Context $betaPackage `
        -Snapshot $betaSnapshot `
        -Enabled $false `
        -Running $false `
        -Version Old
    Assert-ScoopPackageSnapshot `
        -Context $stablePackage `
        -Expected $stableSibling `
        -ExpectedState "RunningEnabled" `
        -Connectable $true
    Invoke-ScoopRestore `
        -Context $betaPackage `
        -Snapshot $betaSnapshot `
        -ExpectedState "StoppedDisabled" `
        -Connectable $false
    Assert-ScoopPackageSnapshot `
        -Context $stablePackage `
        -Expected $stableSibling `
        -ExpectedState "RunningEnabled" `
        -Connectable $true

    $stableForeignSnapshot = Get-ScoopPackageSnapshot -Context $stablePackage
    $betaSibling = Get-ScoopPackageSnapshot -Context $betaPackage
    Invoke-ScoopPrepare `
        -Context $stablePackage `
        -Snapshot $stableForeignSnapshot `
        -Enabled $true `
        -Running $true `
        -Version New
    Move-Item `
        -LiteralPath $stablePackage.StateFile `
        -Destination $stablePackage.RetainedStateFile
    $retainedMarkerHash = Get-Sha256 -Path $stablePackage.RetainedStateFile

    $wrongExecutable = Join-Path $env:SystemRoot "System32\where.exe"
    Register-ForeignScoopTask `
        -Context $stablePackage `
        -Snapshot $stableForeignSnapshot `
        -WrongExecutable $wrongExecutable
    $foreignSnapshot = Get-ScoopPackageSnapshot -Context $stablePackage

    Set-ScoopPackageContext -Context $stablePackage -Version New
    $foreignPrepare = Invoke-TraceDecayRaw -Arguments @(
        "package-hook",
        "scoop",
        "prepare",
        "--package-id",
        $stablePackage.PackageId,
        "--state-file",
        $stablePackage.StateFile
    )
    Assert-CommandSucceeded -Result $foreignPrepare `
        -Description "foreign stable Scoop prepare no-op"
    Assert-ScoopPackageSnapshot `
        -Context $stablePackage `
        -Expected $foreignSnapshot `
        -SkipObservation
    if (Test-Path -LiteralPath $stablePackage.StateFile) {
        throw "foreign stable Scoop prepare created a marker"
    }
    Assert-Equal `
        -Actual (Get-Sha256 -Path $stablePackage.RetainedStateFile) `
        -Expected $retainedMarkerHash `
        -Description "retained valid Scoop marker"

    Move-Item `
        -LiteralPath $stablePackage.RetainedStateFile `
        -Destination $stablePackage.StateFile
    $retainedMarkerHash = Get-Sha256 -Path $stablePackage.StateFile
    Set-ScoopPackageContext -Context $stablePackage -Version New
    $foreignRestore = Invoke-TraceDecayRaw -Arguments @(
        "package-hook",
        "scoop",
        "restore",
        "--package-id",
        $stablePackage.PackageId,
        "--state-file",
        $stablePackage.StateFile
    )
    Write-CommandOutput -Result $foreignRestore
    if ($foreignRestore.ExitCode -eq 0) {
        throw "foreign stable Scoop restore unexpectedly succeeded"
    }
    Assert-ScoopPackageSnapshot `
        -Context $stablePackage `
        -Expected $foreignSnapshot `
        -SkipObservation
    Assert-Equal `
        -Actual (Get-Sha256 -Path $stablePackage.StateFile) `
        -Expected $retainedMarkerHash `
        -Description "failed restore retained Scoop marker"
    if (Test-Path -LiteralPath $stablePackage.RuntimeExecutable) {
        throw "failed foreign restore mutated the stable runtime"
    }
    Assert-ScoopPackageSnapshot `
        -Context $betaPackage `
        -Expected $betaSibling `
        -ExpectedState "StoppedDisabled" `
        -Connectable $false

    $script:taskRoot.DeleteTask($stablePackage.TaskName, 0)
    Invoke-ScoopRestore `
        -Context $stablePackage `
        -Snapshot $stableForeignSnapshot `
        -ExpectedState "RunningEnabled" `
        -Connectable $true
    Assert-ScoopPackageSnapshot `
        -Context $betaPackage `
        -Expected $betaSibling `
        -ExpectedState "StoppedDisabled" `
        -Connectable $false

    $stableUninstallSnapshot = Get-ScoopPackageSnapshot -Context $stablePackage
    Invoke-ScoopPrepare `
        -Context $stablePackage `
        -Snapshot $stableUninstallSnapshot `
        -Enabled $true `
        -Running $true `
        -Version New
    Assert-ScoopPackageSnapshot `
        -Context $betaPackage `
        -Expected $betaSibling `
        -ExpectedState "StoppedDisabled" `
        -Connectable $false
    Remove-ScoopUninstallMarker -Context $stablePackage
    Assert-ScoopPackageAbsent -Context $stablePackage

    $betaUninstallSnapshot = Get-ScoopPackageSnapshot -Context $betaPackage
    Invoke-ScoopPrepare `
        -Context $betaPackage `
        -Snapshot $betaUninstallSnapshot `
        -Enabled $false `
        -Running $false `
        -Version New
    Assert-ScoopPackageAbsent -Context $stablePackage
    Remove-ScoopUninstallMarker -Context $betaPackage
    Assert-ScoopPackageAbsent -Context $betaPackage
    Write-Host "Windows Scoop service lifecycle E2E passed"
}
catch {
    $primaryError = $_
}
finally {
    $taskProvenRemoved = $false
    try {
        foreach ($taskName in $script:cleanupTaskNames) {
            $taskPath = "\$taskName"
            $task = Get-TaskAtPathOrNull -TaskPath $taskPath
            if ($null -ne $task) {
                try {
                    $task.Stop(0)
                }
                catch {
                    Write-Warning "fallback task stop failed: $($_.Exception.Message)"
                }
                $script:taskRoot.DeleteTask($taskName, 0)
            }
            if ($null -ne (Get-TaskAtPathOrNull -TaskPath $taskPath)) {
                throw "cleanup left scheduled task '$taskPath' registered"
            }
        }
        $taskProvenRemoved = $true

        foreach ($context in $script:packageContexts) {
            if (Test-Path -LiteralPath $context.RuntimeDirectory) {
                Remove-Item -LiteralPath $context.RuntimeDirectory -Recurse -Force
            }
        }
    }
    catch {
        $cleanupError = $_
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
        Remove-Item -LiteralPath $testRoot -Recurse -Force
    }
    else {
        Write-Warning "retained service E2E root for diagnosis: $testRoot"
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
