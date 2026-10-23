$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false

$target = "//crates/tracedecay:transcript_ingest_suite__binary"
$diagnostics = Join-Path $PWD "diagnostics/windows-crate-inputs"
New-Item -ItemType Directory -Force -Path $diagnostics | Out-Null

$buildLog = Join-Path $diagnostics "build.log"
& bazel build --config=ci --verbose_failures --subcommands=pretty_print $target *> $buildLog
$buildExitCode = $LASTEXITCODE
Set-Content -Path (Join-Path $diagnostics "build-exit-code.txt") -Value $buildExitCode

$queryLog = Join-Path $diagnostics "rustc-actions.json"
$queryError = Join-Path $diagnostics "aquery.stderr.log"
$expression = 'mnemonic("Rustc", //crates/tracedecay:transcript_ingest_suite__binary)'
& bazel aquery --config=ci --output=jsonproto --include_artifacts --include_commandline $expression 1> $queryLog 2> $queryError
$queryExitCode = $LASTEXITCODE
Set-Content -Path (Join-Path $diagnostics "aquery-exit-code.txt") -Value $queryExitCode

$captureExitCode = 0
try {
    $query = Get-Content -Raw $queryLog | ConvertFrom-Json
    $targetLabels = @{}
    foreach ($targetRecord in @($query.targets)) {
        $targetLabels[[string]$targetRecord.id] = $targetRecord.label
    }
    $action = @($query.actions | Where-Object {
        $_.mnemonic -eq "Rustc" -and
        $targetLabels[[string]$_.targetId] -eq $target
    } | Select-Object -First 1)

    if ($action.Count -eq 0) {
        throw "aquery did not return the target's Rustc action"
    }
    $action = $action[0]

    $action | ConvertTo-Json -Depth 100 | Set-Content -Path (Join-Path $diagnostics "target-rustc-action.json")
    $arguments = @($action.arguments)
    $argumentLines = for ($index = 0; $index -lt $arguments.Count; $index++) {
        "[$index] $($arguments[$index])"
    }
    Set-Content -Path (Join-Path $diagnostics "rustc-argv.txt") -Value $argumentLines

    $executionRootPath = Join-Path $diagnostics "execution-root.txt"
    $executionRootError = Join-Path $diagnostics "execution-root.stderr.log"
    & bazel info execution_root 1> $executionRootPath 2> $executionRootError
    $executionRootExitCode = $LASTEXITCODE
    Set-Content -Path (Join-Path $diagnostics "execution-root-exit-code.txt") -Value $executionRootExitCode
    $executionRoot = (Get-Content -Raw $executionRootPath).Trim()
    if ($executionRootExitCode -ne 0 -or -not $executionRoot) {
        throw "could not resolve Bazel's execution root"
    }

    $externs = @{}
    for ($index = 0; $index -lt $arguments.Count; $index++) {
        $mapping = $null
        if ($arguments[$index] -eq "--extern" -and $index + 1 -lt $arguments.Count) {
            $mapping = $arguments[$index + 1]
            $index++
        } elseif ($arguments[$index] -match '^--extern=(.+)$') {
            $mapping = $Matches[1]
        }

        if ($mapping -and $mapping -match '^([^=]+)=(.+)$') {
            $externs[$Matches[1]] = $Matches[2]
        }
    }

    $presence = foreach ($crate in @("tracedecay", "tracedecay_mcp", "tracedecay_project")) {
        if (-not $externs.ContainsKey($crate)) {
            "$crate extern=absent"
            continue
        }

        $artifact = $externs[$crate]
        if (-not [IO.Path]::IsPathRooted($artifact)) {
            $artifact = Join-Path $executionRoot $artifact
        }

        if (-not (Test-Path -LiteralPath $artifact -PathType Leaf)) {
            "$crate extern=$($externs[$crate]) exists=false readable=false"
            continue
        }

        $stream = $null
        try {
            $stream = [IO.File]::Open($artifact, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
            "$crate extern=$($externs[$crate]) exists=true readable=true length=$($stream.Length)"
        } catch {
            "$crate extern=$($externs[$crate]) exists=true readable=false error=$($_.Exception.Message)"
        } finally {
            if ($stream) {
                $stream.Dispose()
            }
        }
    }
    Set-Content -Path (Join-Path $diagnostics "extern-artifact-presence.txt") -Value $presence
} catch {
    $captureExitCode = 1
    Set-Content -Path (Join-Path $diagnostics "capture-error.txt") -Value $_.Exception.Message
}

Set-Content -Path (Join-Path $diagnostics "capture-exit-code.txt") -Value $captureExitCode
if ($buildExitCode -ne 0) {
    exit $buildExitCode
}
if ($queryExitCode -ne 0) {
    exit $queryExitCode
}
exit $captureExitCode
