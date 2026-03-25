# AgentFS Windows AppFS managed lifecycle regression test
# Covers: WinFsp managed mount + serve appfs --managed + register_app + enter_scope + snapshot read + unregister_app

param(
    [string]$AgentId = "win-managed-regression",
    [string]$MountPoint = "C:\mnt\win-managed-regression",
    [string]$AppId = "aiim",
    [string]$HttpEndpoint = "http://127.0.0.1:8080",
    [switch]$SkipCleanup,
    [switch]$KeepLogs
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$script:RepoRoot = Split-Path -Parent $PSScriptRoot
$script:DbPath = Join-Path $PSScriptRoot ".agentfs\$AgentId.db"
$script:MountHandle = $null
$script:RuntimeHandle = $null
$script:BridgeHandle = $null
$script:Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$script:LogDir = Join-Path ([System.IO.Path]::GetTempPath()) ("agentfs-win-managed-{0}-{1}" -f $AgentId, ([guid]::NewGuid().ToString("N")))
$script:ProgressLog = $null

function Write-Success { Write-Host "✓ $args" -ForegroundColor Green }
function Write-Fail { Write-Host "✗ $args" -ForegroundColor Red }
function Write-WarningLine { Write-Host "⚠ $args" -ForegroundColor Yellow }
function Write-Info { Write-Host "ℹ $args" -ForegroundColor Cyan }
function Write-Section { Write-Host "`n==== $args ====" -ForegroundColor Magenta }

function Write-ProgressLog {
    param([string]$Message)

    if ($script:ProgressLog) {
        [System.IO.File]::AppendAllText(
            $script:ProgressLog,
            ("[{0}] {1}`n" -f (Get-Date).ToString("HH:mm:ss.fff"), $Message),
            $script:Utf8NoBom
        )
    }
}

function Remove-TestPath {
    param(
        [string]$Path,
        [switch]$Recurse
    )

    if (!(Test-Path $Path)) {
        return
    }

    try {
        Remove-Item -Path $Path -Force -Recurse:$Recurse -ErrorAction Stop
    } catch {
        if (Test-Path $Path -PathType Container) {
            if ($Recurse) {
                cmd /c "rmdir /s /q `"$Path`"" | Out-Null
            } else {
                cmd /c "rmdir `"$Path`"" | Out-Null
            }
        } elseif (Test-Path $Path -PathType Leaf) {
            cmd /c "del /f /q `"$Path`"" | Out-Null
        }
    }
}

function Stop-LoggedProcess {
    param($Handle)

    if ($null -eq $Handle) {
        return
    }

    if ($Handle.Process -and !$Handle.Process.HasExited) {
        try {
            Stop-Process -Id $Handle.Process.Id -Force -ErrorAction Stop
            $null = $Handle.Process.WaitForExit(5000)
        } catch {
            Write-WarningLine "Failed to stop $($Handle.Name): $_"
        }
    }
}

function Cleanup-TestArtifacts {
    Stop-LoggedProcess $script:RuntimeHandle
    Stop-LoggedProcess $script:MountHandle
    Stop-LoggedProcess $script:BridgeHandle

    if (!$SkipCleanup) {
        Remove-TestPath -Path $MountPoint -Recurse
        Remove-TestPath -Path $script:DbPath
        Remove-TestPath -Path "$($script:DbPath)-shm"
        Remove-TestPath -Path "$($script:DbPath)-wal"
    }

    if (!$KeepLogs -and (Test-Path $script:LogDir)) {
        Remove-TestPath -Path $script:LogDir -Recurse
    }
}

function Fail-WithContext {
    param([string]$Message)

    Write-Fail $Message
    if (Test-Path $script:LogDir) {
        Write-Host "`nLogs preserved at $script:LogDir" -ForegroundColor Gray
        foreach ($path in @(
            (Join-Path $script:LogDir "bridge.stdout.log"),
            (Join-Path $script:LogDir "bridge.stderr.log"),
            (Join-Path $script:LogDir "mount.stdout.log"),
            (Join-Path $script:LogDir "mount.stderr.log"),
            (Join-Path $script:LogDir "runtime.stdout.log"),
            (Join-Path $script:LogDir "runtime.stderr.log")
        )) {
            if (Test-Path $path) {
                Write-Host "`n--- tail: $path ---" -ForegroundColor Gray
                Get-Content $path -Tail 20 -ErrorAction SilentlyContinue
            }
        }
    }
    throw $Message
}

function New-LogHandle {
    param(
        [string]$Name,
        [string]$FilePath,
        [string[]]$ArgumentList,
        [string]$WorkingDirectory
    )

    $stdout = Join-Path $script:LogDir "$Name.stdout.log"
    $stderr = Join-Path $script:LogDir "$Name.stderr.log"
    $proc = Start-Process -FilePath $FilePath `
        -ArgumentList $ArgumentList `
        -WorkingDirectory $WorkingDirectory `
        -PassThru `
        -WindowStyle Hidden `
        -RedirectStandardOutput $stdout `
        -RedirectStandardError $stderr
    return [pscustomobject]@{
        Name = $Name
        Process = $proc
        Stdout = $stdout
        Stderr = $stderr
    }
}

function Assert-True {
    param(
        [bool]$Condition,
        [string]$Message
    )

    if (!$Condition) {
        Fail-WithContext $Message
    }
    Write-Success $Message
}

function Wait-Until {
    param(
        [scriptblock]$Condition,
        [string]$Description,
        [int]$TimeoutSec = 15
    )

    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    $lastError = $null
    while ((Get-Date) -lt $deadline) {
        try {
            if (& $Condition) {
                return
            }
        } catch {
            $lastError = $_.Exception.Message
        }
        Start-Sleep -Milliseconds 250
    }

    if ($lastError) {
        Fail-WithContext "Timed out waiting for $Description. Last error: $lastError"
    } else {
        Fail-WithContext "Timed out waiting for $Description"
    }
}

function Ensure-ProcessRunning {
    param($Handle)

    if ($Handle.Process.HasExited) {
        throw "$($Handle.Name) exited unexpectedly with code $($Handle.Process.ExitCode)"
    }
}

function Append-Utf8JsonLine {
    param(
        [string]$Path,
        [string]$JsonLine
    )

    $parent = Split-Path -Parent $Path
    if ($parent) {
        [void][System.IO.Directory]::CreateDirectory($parent)
    }
    [System.IO.File]::AppendAllText($Path, $JsonLine + "`n", $script:Utf8NoBom)
}

function Read-JsonFile {
    param([string]$Path)
    return (Get-Content -Path $Path -Raw -ErrorAction Stop | ConvertFrom-Json -ErrorAction Stop)
}

function Read-DbVirtualJsonFile {
    param([string]$VirtualPath)

    Push-Location $PSScriptRoot
    try {
        $output = & cargo run --quiet -- fs $script:DbPath cat $VirtualPath 2>$null
        if ($LASTEXITCODE -ne 0) {
            return $null
        }
        $json = ($output -join "`n").Trim()
        if ([string]::IsNullOrWhiteSpace($json)) {
            return $null
        }
        return ($json | ConvertFrom-Json -ErrorAction Stop)
    } finally {
        Pop-Location
    }
}

function Find-EventLine {
    param(
        [string]$Path,
        [string]$ClientToken,
        [string]$ExpectedType
    )

    if (!(Test-Path $Path)) {
        return $null
    }

    foreach ($line in (Get-Content -Path $Path -ErrorAction SilentlyContinue)) {
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }
        try {
            $event = $line | ConvertFrom-Json -ErrorAction Stop
        } catch {
            continue
        }
        if ($event.client_token -eq $ClientToken -and $event.type -eq $ExpectedType) {
            return $event
        }
    }

    return $null
}

function Wait-Event {
    param(
        [string]$Path,
        [string]$ClientToken,
        [string]$ExpectedType,
        [int]$TimeoutSec = 20
    )

    Wait-Until -Description "$ExpectedType for token $ClientToken" -TimeoutSec $TimeoutSec -Condition {
        $null -ne (Find-EventLine -Path $Path -ClientToken $ClientToken -ExpectedType $ExpectedType)
    }
    return Find-EventLine -Path $Path -ClientToken $ClientToken -ExpectedType $ExpectedType
}

function Refresh-ParentDirectory {
    param([string]$Path)

    $parent = Split-Path -Parent $Path
    if ($parent -and (Test-Path $parent -PathType Container)) {
        try {
            Get-ChildItem -Path $parent -ErrorAction Stop | Out-Null
        } catch {
            # Best-effort directory refresh for WinFsp path visibility.
        }
    }
}

function Wait-Path {
    param(
        [string]$Path,
        [string]$Description,
        [int]$TimeoutSec = 15,
        [switch]$Directory
    )

    Wait-Until -Description $Description -TimeoutSec $TimeoutSec -Condition {
        Refresh-ParentDirectory -Path $Path
        if ($Directory) {
            return Test-Path -Path $Path -PathType Container
        }
        return Test-Path -Path $Path
    }
}

function Wait-PathMissing {
    param(
        [string]$Path,
        [string]$Description,
        [int]$TimeoutSec = 15
    )

    Wait-Until -Description $Description -TimeoutSec $TimeoutSec -Condition {
        Refresh-ParentDirectory -Path $Path
        return !(Test-Path -Path $Path)
    }
}

function Wait-JsonCondition {
    param(
        [string]$Path,
        [scriptblock]$Condition,
        [string]$Description,
        [int]$TimeoutSec = 20
    )

    Wait-Until -Description $Description -TimeoutSec $TimeoutSec -Condition {
        $json = Read-JsonFile -Path $Path
        return (& $Condition $json)
    }
}

function Wait-DbJsonCondition {
    param(
        [string]$VirtualPath,
        [scriptblock]$Condition,
        [string]$Description,
        [int]$TimeoutSec = 30
    )

    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    $lastError = $null
    while ((Get-Date) -lt $deadline) {
        try {
            $json = Read-DbVirtualJsonFile -VirtualPath $VirtualPath
            if ($null -ne $json -and (& $Condition $json)) {
                return
            }
        } catch {
            $lastError = $_.Exception.Message
        }
        Start-Sleep -Seconds 1
    }

    if ($lastError) {
        Fail-WithContext "Timed out waiting for $Description. Last error: $lastError"
    } else {
        Fail-WithContext "Timed out waiting for $Description"
    }
}

function Read-FirstNonEmptyLines {
    param(
        [string]$Path,
        [int]$Count = 5
    )

    $raw = [System.IO.File]::ReadAllText($Path, $script:Utf8NoBom)
    $lines = @(
        ($raw -split "`r?`n") |
            Where-Object { $_.Trim().Length -gt 0 } |
            Select-Object -First $Count
    )
    return ,@($lines)
}

function Wait-ReadableSnapshot {
    param(
        [string]$Path,
        [string]$Description,
        [int]$TimeoutSec = 20
    )

    Wait-Until -Description $Description -TimeoutSec $TimeoutSec -Condition {
        try {
            Refresh-ParentDirectory -Path $Path
            $lines = Read-FirstNonEmptyLines -Path $Path -Count 1
            Write-ProgressLog ("snapshot probe {0}: {1} line(s)" -f $Path, $lines.Count)
            return $lines.Count -gt 0
        } catch {
            Write-ProgressLog ("snapshot probe {0} failed: {1}" -f $Path, $_.Exception.Message)
            return $false
        }
    }
}

function Main {
    New-Item -ItemType Directory -Path $script:LogDir -Force | Out-Null
    $script:ProgressLog = Join-Path $script:LogDir "progress.log"
    Write-ProgressLog "starting managed lifecycle regression"

    $mountParent = Split-Path -Parent $MountPoint
    if ($mountParent -and !(Test-Path $mountParent)) {
        New-Item -ItemType Directory -Path $mountParent -Force | Out-Null
    }

    if (Test-Path $MountPoint) {
        Remove-TestPath -Path $MountPoint -Recurse
    }
    Remove-TestPath -Path $script:DbPath
    Remove-TestPath -Path "$($script:DbPath)-shm"
    Remove-TestPath -Path "$($script:DbPath)-wal"

    Write-Section "Start HTTP Bridge"
    Write-ProgressLog "starting http bridge"
    $bridgeUri = [Uri]$HttpEndpoint
    $bridgeHost = $bridgeUri.Host
    $bridgePort = $bridgeUri.Port
    $bridgeDir = Join-Path $script:RepoRoot "examples\appfs\http-bridge\python"
    $bridgeCommand = "set `"APPFS_BRIDGE_HOST=$bridgeHost`" && set `"APPFS_BRIDGE_PORT=$bridgePort`" && python -u bridge_server.py"
    $script:BridgeHandle = New-LogHandle -Name "bridge" -FilePath "cmd.exe" -ArgumentList @("/c", $bridgeCommand) -WorkingDirectory $bridgeDir
    Wait-Until -Description "HTTP bridge startup" -TimeoutSec 15 -Condition {
        Ensure-ProcessRunning $script:BridgeHandle
        try {
            $response = Invoke-RestMethod -Uri "$HttpEndpoint/v2/connector/info" -Method Post -ContentType "application/json" -Body "{}" -ErrorAction Stop
            return $response.app_id -eq "aiim"
        } catch {
            return $false
        }
    }
    Write-Success "HTTP bridge is serving at $HttpEndpoint"
    Write-ProgressLog "http bridge ready"

    Write-Section "Init AgentFS"
    Write-ProgressLog "initializing agentfs db"
    Push-Location $PSScriptRoot
    try {
        & cargo run -- init $AgentId --force
        if ($LASTEXITCODE -ne 0) {
            Fail-WithContext "agentfs init failed"
        }
    } finally {
        Pop-Location
    }
    Assert-True (Test-Path $script:DbPath) "Created database $script:DbPath"
    Write-ProgressLog "db initialized"

    Write-Section "Mount WinFsp Managed Root"
    Write-ProgressLog "starting winfsp mount"
    $mountArgs = @(
        "run", "--",
        "mount", $script:DbPath, $MountPoint,
        "--backend", "winfsp",
        "--foreground",
        "--managed-appfs"
    )
    $script:MountHandle = New-LogHandle -Name "mount" -FilePath "cargo" -ArgumentList $mountArgs -WorkingDirectory $PSScriptRoot
    Wait-Until -Description "WinFsp mount readiness" -TimeoutSec 20 -Condition {
        Ensure-ProcessRunning $script:MountHandle
        if (!(Test-Path $MountPoint -PathType Container)) {
            return $false
        }
        try {
            Get-ChildItem -Path $MountPoint -ErrorAction Stop | Out-Null
            return $true
        } catch {
            return $false
        }
    }
    Write-Success "Managed WinFsp mount is ready at $MountPoint"
    Write-ProgressLog "mount ready"

    Write-Section "Start Managed Runtime"
    Write-ProgressLog "starting managed runtime"
    $runtimeArgs = @("run", "--", "serve", "appfs", "--root", $MountPoint, "--managed")
    $script:RuntimeHandle = New-LogHandle -Name "runtime" -FilePath "cargo" -ArgumentList $runtimeArgs -WorkingDirectory $PSScriptRoot

    $controlDir = Join-Path $MountPoint "_appfs"
    $controlStream = Join-Path $controlDir "_stream"
    $controlEvents = Join-Path $controlStream "events.evt.jsonl"
    $registryPath = Join-Path $controlDir "apps.registry.json"
    Wait-Until -Description "managed control plane bootstrap" -TimeoutSec 20 -Condition {
        Ensure-ProcessRunning $script:RuntimeHandle
        return (Test-Path (Join-Path $controlDir "register_app.act")) -and
            (Test-Path (Join-Path $controlDir "list_apps.act")) -and
            (Test-Path $controlEvents)
    }
    Write-Success "Managed control plane is ready"
    Write-ProgressLog "managed control plane ready"

    Write-Section "Register HTTP App"
    Write-ProgressLog "registering app"
    $registerToken = "reg-http-001"
    $registerAction = Join-Path $controlDir "register_app.act"
    $registerPayload = @{
        app_id = $AppId
        transport = @{
            kind = "http"
            endpoint = $HttpEndpoint
            http_timeout_ms = 5000
            grpc_timeout_ms = 5000
            bridge_max_retries = 2
            bridge_initial_backoff_ms = 100
            bridge_max_backoff_ms = 1000
            bridge_circuit_breaker_failures = 5
            bridge_circuit_breaker_cooldown_ms = 3000
        }
        client_token = $registerToken
    } | ConvertTo-Json -Compress
    Append-Utf8JsonLine -Path $registerAction -JsonLine $registerPayload
    Wait-DbJsonCondition -VirtualPath "/_appfs/apps.registry.json" -Description "registry contains registered app" -Condition {
        param($doc)
        return $doc.apps.Count -eq 1 -and $doc.apps[0].app_id -eq $AppId
    }
    Write-Success "register_app updated the managed registry"
    Write-ProgressLog "register_app observed in registry"

    $appRoot = Join-Path $MountPoint $AppId
    $appEvents = Join-Path $appRoot "_stream\events.evt.jsonl"
    $chat001Snapshot = Join-Path $appRoot "chats\chat-001\messages.res.jsonl"
    Wait-Path -Path $appRoot -Description "registered app root" -Directory
    Wait-Path -Path (Join-Path $appRoot "_app\enter_scope.act") -Description "enter_scope control action"
    Wait-Path -Path $chat001Snapshot -Description "initial snapshot placeholder"
    Wait-ReadableSnapshot -Path $chat001Snapshot -Description "initial snapshot read-through"
    $initialLines = Read-FirstNonEmptyLines -Path $chat001Snapshot -Count 3
    Assert-True ($initialLines.Count -gt 0) "initial scope snapshot is readable"
    Write-ProgressLog "initial snapshot readable"

    Write-Section "Enter Scope And Refresh Structure"
    Write-ProgressLog "entering scope chat-long"
    $enterScopeAction = Join-Path $appRoot "_app\enter_scope.act"
    $enterScopePayload = @{
        target_scope = "chat-long"
        client_token = "scope-http-001"
    } | ConvertTo-Json -Compress
    Append-Utf8JsonLine -Path $enterScopeAction -JsonLine $enterScopePayload
    Wait-DbJsonCondition -VirtualPath "/_appfs/apps.registry.json" -Description "registry active scope updated" -Condition {
        param($doc)
        return $doc.apps.Count -eq 1 -and $doc.apps[0].active_scope -eq "chat-long"
    }
    Write-Success "enter_scope switched active scope to chat-long"
    Write-ProgressLog "registry active scope is chat-long"

    $chat001Dir = Join-Path $appRoot "chats\chat-001"
    $chatLongSnapshot = Join-Path $appRoot "chats\chat-long\messages.res.jsonl"
    Wait-PathMissing -Path $chat001Dir -Description "old scope prune"
    Write-ProgressLog "old scope pruned"
    Wait-Path -Path $chatLongSnapshot -Description "new scope snapshot placeholder"
    Write-ProgressLog "new scope snapshot placeholder visible"
    Wait-ReadableSnapshot -Path $chatLongSnapshot -Description "new scope snapshot read-through"
    $chatLongLines = Read-FirstNonEmptyLines -Path $chatLongSnapshot -Count 3
    Assert-True ($chatLongLines.Count -gt 0) "new scope snapshot is readable after enter_scope"
    Write-ProgressLog "chat-long scope readable"

    Write-Section "Unregister App"
    Write-ProgressLog "unregistering app"
    $unregisterAction = Join-Path $controlDir "unregister_app.act"
    $unregisterPayload = @{
        app_id = $AppId
        client_token = "unreg-http-001"
    } | ConvertTo-Json -Compress
    Append-Utf8JsonLine -Path $unregisterAction -JsonLine $unregisterPayload
    Wait-DbJsonCondition -VirtualPath "/_appfs/apps.registry.json" -Description "registry is empty after unregister" -Condition {
        param($doc)
        return $doc.apps.Count -eq 0
    }
    Write-Success "unregister_app removed the app from managed registry"
    Assert-True (Test-Path $appRoot -PathType Container) "unregister_app keeps app tree for inspection"
    Write-ProgressLog "unregister succeeded"

    Write-Host "`nManaged lifecycle regression test passed." -ForegroundColor Green
    Write-ProgressLog "test passed"
}

try {
    Main
} catch {
    Write-Fail $_
    if (!$KeepLogs) {
        Write-Host "Preserving logs because the test failed: $script:LogDir" -ForegroundColor Yellow
        $KeepLogs = $true
    }
    exit 1
} finally {
    Cleanup-TestArtifacts
}
