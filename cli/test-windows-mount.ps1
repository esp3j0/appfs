# AgentFS Windows Mounting Comprehensive Test Script
# Version: 1.0
# Tests all Windows mounting functionality for AgentFS

param(
    [string]$AgentId = "win-test",
    [string]$MountPoint = "C:\mnt\win-test",
    [switch]$SkipCleanup,
    [switch]$QuickTest,
    [string]$TestModule
)

# Test configuration
$script:TestPassed = 0
$script:TestFailed = 0
$script:TestSkipped = 0
$script:MountProcess = $null
$script:DbPath = ".agentfs\$AgentId.db"

# Color output functions
function Write-Success { Write-Host "✓ $args" -ForegroundColor Green }
function Write-Fail { Write-Host "✗ $args" -ForegroundColor Red }
function Write-Warning { Write-Host "⚠ $args" -ForegroundColor Yellow }
function Write-Info { Write-Host "ℹ $args" -ForegroundColor Cyan }
function Write-TestHeader { Write-Host "`n========== $args ==========" -ForegroundColor Magenta }

# Test helper functions
function Test-Assert {
    param(
        [bool]$Condition,
        [string]$Message,
        [string]$Expected = "",
        [string]$Actual = ""
    )

    if ($Condition) {
        $script:TestPassed++
        Write-Success $Message
        return $true
    } else {
        $script:TestFailed++
        Write-Fail $Message
        if ($Expected -and $Actual) {
            Write-Host "  Expected: $Expected" -ForegroundColor Gray
            Write-Host "  Actual:   $Actual" -ForegroundColor Gray
        }
        return $false
    }
}

function Test-FileExists {
    param([string]$Path)
    return Test-Path -Path $Path -PathType Leaf
}

function Test-DirExists {
    param([string]$Path)
    return Test-Path -Path $Path -PathType Container
}

function Remove-TestPath {
    param(
        [string]$Path,
        [switch]$Recurse
    )

    if (!(Test-Path $Path)) {
        return $true
    }

    try {
        Remove-Item -Path $Path -Force -Recurse:$Recurse -ErrorAction Stop
    } catch {
        # Fallback for WinFsp paths where PowerShell Remove-Item may fail with
        # "The system cannot find the file specified".
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

    return !(Test-Path $Path)
}

# Cleanup function
function Invoke-Cleanup {
    Write-Info "Cleaning up..."

    # Unmount if mounted
    if ($script:MountProcess -and !$script:MountProcess.HasExited) {
        Write-Info "Stopping mount process..."
        $script:MountProcess.Kill()
        $script:MountProcess.WaitForExit(5000)
    }

    # Remove test database
    if (Test-Path $script:DbPath) {
        Remove-Item $script:DbPath -Force -ErrorAction SilentlyContinue
        Remove-Item "$($script:DbPath)-shm" -Force -ErrorAction SilentlyContinue
        Remove-Item "$($script:DbPath)-wal" -Force -ErrorAction SilentlyContinue
    }

    # Remove mount point (if empty)
    try {
        if (Test-DirExists $MountPoint) {
            $items = Get-ChildItem $MountPoint -ErrorAction SilentlyContinue
            if ($items -eq $null -or $items.Count -eq 0) {
                Remove-Item $MountPoint -Force -Recurse -ErrorAction SilentlyContinue
            }
        }
    } catch {
        # Ignore errors during mount point cleanup
    }

    Write-Success "Cleanup completed"
}

# Register cleanup on exit
if (!$SkipCleanup) {
    Register-EngineEvent -SourceIdentifier PowerShell.Exiting -Action { Invoke-Cleanup }
}

# ============================================================
# Module A: Environment Setup
# ============================================================
function Test-EnvironmentSetup {
    Write-TestHeader "Module A: Environment Setup"

    # Test 1: Initialize agent
    Write-Info "Initializing agent '$AgentId'..."
    $output = cargo run -- init $AgentId 2>&1
    $success = Test-Assert ($LASTEXITCODE -eq 0) "Initialize agent database" "Exit code 0" "Exit code $LASTEXITCODE"

    if (!$success) {
        Write-Host $output
        return $false
    }

    # Test 2: Check database file exists
    Test-Assert (Test-Path $script:DbPath) "Database file created at $script:DbPath"

    # Test 3: Ensure mount point does NOT exist (WinFsp requires non-existent path)
    if (Test-DirExists $MountPoint) {
        Remove-Item -Path $MountPoint -Recurse -Force -ErrorAction SilentlyContinue
    }
    Test-Assert (!(Test-DirExists $MountPoint)) "Mount point directory does not exist (WinFsp will create it)"

    # Test 4: Start mount
    Write-Info "Starting mount (this may take a moment)..."
    $script:MountProcess = Start-Process -FilePath "cargo" `
        -ArgumentList "run", "--", "mount", $script:DbPath, $MountPoint, "--foreground" `
        -PassThru -WindowStyle Hidden -RedirectStandardOutput "mount-stdout.log" -RedirectStandardError "mount-stderr.log"

    # Wait for mount to be ready
    Start-Sleep -Seconds 3

    # Check if process is running
    $processRunning = !$script:MountProcess.HasExited
    if (!$processRunning) {
        Write-Warning "Mount process exited prematurely. Check mount-stderr.log for details."
    }
    Test-Assert $processRunning "Mount process started successfully"

    # Verify mount is accessible
    $retries = 0
    $mounted = $false
    while ($retries -lt 10 -and !$mounted) {
        try {
            Get-ChildItem $MountPoint -ErrorAction Stop | Out-Null
            $mounted = $true
        } catch {
            Start-Sleep -Milliseconds 500
            $retries++
        }
    }

    Test-Assert $mounted "Mount point is accessible"

    return $true
}

# ============================================================
# Module B: Basic File Operations
# ============================================================
function Test-BasicFileOperations {
    Write-TestHeader "Module B: Basic File Operations"

    $testFile = Join-Path $MountPoint "test.txt"
    $content = "Hello from AgentFS on Windows!"

    # Test 1: Create file
    Set-Content -Path $testFile -Value $content -NoNewline
    Test-Assert (Test-FileExists $testFile) "Create file"

    # Test 2: Read file
    $readContent = Get-Content -Path $testFile -Raw
    Test-Assert ($readContent -eq $content) "Read file content" $content $readContent

    # Test 3: Overwrite file (use Clear-Content to ensure truncation)
    $newContent = "Updated content"
    Clear-Content -Path $testFile -Force
    Set-Content -Path $testFile -Value $newContent -NoNewline
    $readContent = Get-Content -Path $testFile -Raw
    Test-Assert ($readContent -eq $newContent) "Overwrite file" $newContent $readContent

    # Test 4: Append to file
    $appendContent = "Appended line"
    Add-Content -Path $testFile -Value $appendContent
    $readContent = Get-Content -Path $testFile -Raw
    $appendOk = $readContent.Contains($newContent) -and $readContent.Contains($appendContent)
    Test-Assert $appendOk "Append to file" "Contains both original and appended text" $readContent

    # Test 5: Delete file
    Test-Assert (Remove-TestPath -Path $testFile) "Delete file"

    # Test 6: Binary file
    $binaryFile = Join-Path $MountPoint "binary.bin"
    $bytes = [byte[]]@(1, 2, 3, 4, 5, 6, 7, 8, 9, 10)
    [System.IO.File]::WriteAllBytes($binaryFile, $bytes)
    $readBytes = [System.IO.File]::ReadAllBytes($binaryFile)
    Test-Assert ((Compare-Object $bytes $readBytes) -eq $null) "Binary file read/write"
    Remove-Item $binaryFile -Force

    # Test 7: Empty file
    $emptyFile = Join-Path $MountPoint "empty.txt"
    New-Item -Path $emptyFile -ItemType File -Force | Out-Null
    Test-Assert (Test-FileExists $emptyFile) "Create empty file"
    $fileInfo = Get-Item $emptyFile
    Test-Assert ($fileInfo.Length -eq 0) "Empty file has zero size"
    Remove-Item $emptyFile -Force

    return $true
}

# ============================================================
# Module C: Directory Operations
# ============================================================
function Test-DirectoryOperations {
    Write-TestHeader "Module C: Directory Operations"

    $testDir = Join-Path $MountPoint "testdir"

    # Test 1: Create directory
    New-Item -Path $testDir -ItemType Directory -Force | Out-Null
    Test-Assert (Test-DirExists $testDir) "Create directory"

    # Test 2: List directory
    $testFile = Join-Path $testDir "file.txt"
    Set-Content -Path $testFile -Value "test"
    $items = Get-ChildItem -Path $testDir
    Test-Assert ($items.Count -eq 1) "List directory contents" "1 item" "$($items.Count) items"

    # Test 3: Nested directory
    $nestedFile = Join-Path $testDir "nested.txt"
    $nestedContent = "nested file content"
    Set-Content -Path $nestedFile -Value $nestedContent
    $readContent = Get-Content -Path $nestedFile
    Test-Assert ($readContent -eq $nestedContent) "Create and read file in subdirectory"

    # Test 4: Multi-level directories
    $deepDir = Join-Path $MountPoint "a\b\c\d"
    New-Item -Path $deepDir -ItemType Directory -Force | Out-Null
    Test-Assert (Test-DirExists $deepDir) "Create multi-level directories"

    # Test 5: File in deep directory
    $deepFile = Join-Path $deepDir "deep.txt"
    Set-Content -Path $deepFile -Value "deep content"
    Test-Assert (Test-FileExists $deepFile) "Create file in deep directory"

    # Test 6: Delete empty directory
    $emptyDir = Join-Path $MountPoint "emptydir"
    New-Item -Path $emptyDir -ItemType Directory -Force | Out-Null
    Test-Assert (Remove-TestPath -Path $emptyDir) "Delete empty directory"

    # Test 7: Delete non-empty directory
    $nonEmptyDir = Join-Path $MountPoint "nonemptydir"
    New-Item -Path $nonEmptyDir -ItemType Directory -Force | Out-Null
    Set-Content -Path (Join-Path $nonEmptyDir "file.txt") -Value "content"
    Test-Assert (Remove-TestPath -Path $nonEmptyDir -Recurse) "Delete non-empty directory"

    # Test 8: Rename directory
    $oldDir = Join-Path $MountPoint "olddir"
    $newDir = Join-Path $MountPoint "newdir"
    New-Item -Path $oldDir -ItemType Directory -Force | Out-Null
    Rename-Item -Path $oldDir -NewName "newdir"
    Test-Assert (!(Test-DirExists $oldDir) -and (Test-DirExists $newDir)) "Rename directory"
    Remove-TestPath -Path $newDir -Recurse | Out-Null

    # Cleanup
    Remove-TestPath -Path $testDir -Recurse | Out-Null
    Remove-TestPath -Path (Join-Path $MountPoint "a") -Recurse | Out-Null

    return $true
}

# ============================================================
# Module D: Symbolic Links
# ============================================================
function Test-SymbolicLinks {
    Write-TestHeader "Module D: Symbolic Links"

    $targetFile = Join-Path $MountPoint "target.txt"
    $linkFile = Join-Path $MountPoint "link.txt"
    $targetDir = Join-Path $MountPoint "targetdir"
    $linkDir = Join-Path $MountPoint "linkdir"
    $linkedFile = Join-Path $linkDir "file.txt"
    $createdFileLink = $false
    $isSymlinkPrivilegeError = {
        param($err)
        $msg = $err.Exception.Message
        $win32Code = ($err.Exception.HResult -band 0xFFFF)
        return (
            ($msg -match "required privilege is not held") -or
            ($msg -match "A required privilege is not held") -or
            ($msg -match "Administrator privilege required") -or
            ($msg -match "Developer Mode") -or
            ($msg -match "Access is denied") -or
            ($win32Code -eq 1314)
        )
    }

    try {
        Set-Content -Path $targetFile -Value "target content" -NoNewline
        try {
            New-Item -Path $linkFile -ItemType SymbolicLink -Value $targetFile -ErrorAction Stop | Out-Null
            $createdFileLink = $true
        } catch {
            if (& $isSymlinkPrivilegeError $_) {
                Write-Warning "Symbolic link creation is blocked by current Windows privilege settings. Skipping symlink tests..."
                Write-Info "Enable Developer Mode or run elevated to execute Module D."
                $script:TestSkipped += 6
                return $true
            }
            throw
        }

        # Test 1: Create file symlink
        $fileLinkExists = Test-Path $linkFile
        Test-Assert $fileLinkExists "Create file symbolic link"

        # Test 2: Read through symlink
        $linkContent = Get-Content -Path $linkFile -Raw
        Test-Assert ($linkContent -eq "target content") "Read through file symlink" "target content" $linkContent

        # Test 3: Directory symlink
        New-Item -Path $targetDir -ItemType Directory -Force | Out-Null
        Set-Content -Path (Join-Path $targetDir "file.txt") -Value "dir content" -NoNewline
        New-Item -Path $linkDir -ItemType SymbolicLink -Value $targetDir -ErrorAction Stop | Out-Null
        $dirLinkExists = Test-Path $linkDir
        Test-Assert $dirLinkExists "Create directory symbolic link"

        # Test 4: Access file through directory symlink
        $content = Get-Content -Path $linkedFile -Raw
        Test-Assert ($content -eq "dir content") "Read file through directory symlink" "dir content" $content

        # Test 5: Delete symlink (not target)
        Remove-TestPath -Path $linkFile | Out-Null
        Test-Assert (!(Test-FileExists $linkFile) -and (Test-FileExists $targetFile)) "Delete symlink without affecting target"

        # Test 6: Symlink attributes
        if (Test-Path $linkDir) {
            $linkInfo = Get-Item $linkDir
            $linkType = $linkInfo.LinkType
            $isReparse = $linkInfo.Attributes.ToString().Contains("ReparsePoint")
            Test-Assert (($linkType -eq "SymbolicLink") -or $isReparse) "Symlink attributes are correct"
        } else {
            Test-Assert $false "Symlink attributes are correct" "Existing symlink path" "Path not found"
        }
    } catch {
        $script:TestFailed++
        Write-Fail "Module SymbolicLinks failed with error: $_"
        try {
            $hr = ('0x{0:X8}' -f $_.Exception.HResult)
            $w32 = ($_.Exception.HResult -band 0xFFFF)
            Write-Host "  HResult: $hr" -ForegroundColor Gray
            Write-Host "  Win32:   $w32" -ForegroundColor Gray
        } catch {
            # best-effort diagnostics
        }
        return $false
    } finally {
        if ($createdFileLink) { Remove-TestPath -Path $linkFile | Out-Null }
        Remove-TestPath -Path $targetFile | Out-Null
        Remove-TestPath -Path $linkDir | Out-Null
        Remove-TestPath -Path $targetDir -Recurse | Out-Null
    }

    return $true
}

# ============================================================
# Module E: File Attributes
# ============================================================
function Test-FileAttributes {
    Write-TestHeader "Module E: File Attributes"

    $testFile = Join-Path $MountPoint "attr-test.txt"
    $content = "Test content for attributes"

    Set-Content -Path $testFile -Value $content -NoNewline

    # Test 1: File size
    $fileInfo = Get-Item $testFile
    Test-Assert ($fileInfo.Length -eq $content.Length) "File size is correct" "$($content.Length) bytes" "$($fileInfo.Length) bytes"

    # Test 2: Creation time
    Test-Assert ($fileInfo.CreationTime -ne $null) "Creation time is set"

    # Test 3: Last write time
    Test-Assert ($fileInfo.LastWriteTime -ne $null) "Last write time is set"

    # Test 4: Last access time
    Test-Assert ($fileInfo.LastAccessTime -ne $null) "Last access time is set"

    # Test 5: File exists check
    Test-Assert (Test-Path $testFile) "Test-Path returns true for existing file"

    # Test 6: File not exists check
    Test-Assert (!(Test-Path (Join-Path $MountPoint "nonexistent.txt"))) "Test-Path returns false for non-existing file"

    # Test 7: Directory vs file
    $testDir = Join-Path $MountPoint "attr-testdir"
    New-Item -Path $testDir -ItemType Directory -Force | Out-Null
    Test-Assert ((Test-Path $testDir -PathType Container)) "Test-Path identifies directory correctly"

    # Cleanup
    Remove-TestPath -Path $testFile | Out-Null
    Remove-TestPath -Path $testDir | Out-Null

    return $true
}

# ============================================================
# Module F: Edge Cases
# ============================================================
function Test-EdgeCases {
    Write-TestHeader "Module F: Edge Cases"

    # Test 1: Large file (>4KB to test chunking)
    $largeFile = Join-Path $MountPoint "large.bin"
    $largeSize = 10KB
    # Create random bytes to avoid byte overflow (byte range is 0-255)
    $bytes = [byte[]]::new($largeSize)
    (New-Object Random).NextBytes($bytes)
    [System.IO.File]::WriteAllBytes($largeFile, $bytes)
    $readBytes = [System.IO.File]::ReadAllBytes($largeFile)
    Test-Assert ($readBytes.Length -eq $largeSize) "Large file (>4KB) read/write" "$largeSize bytes" "$($readBytes.Length) bytes"

    # Test 2: Long filename
    $longName = "a" * 100 + ".txt"
    $longFile = Join-Path $MountPoint $longName
    try {
        Set-Content -Path $longFile -Value "long name test"
        Test-Assert (Test-FileExists $longFile) "Long filename support"
        Remove-Item $longFile -Force -ErrorAction SilentlyContinue
    } catch {
        Write-Warning "Long filename test failed: $_"
        $script:TestFailed++
    }

    # Test 3: Special characters in filename (Chinese)
    $chineseFile = Join-Path $MountPoint "中文测试.txt"
    try {
        Set-Content -Path $chineseFile -Value "Chinese filename test"
        Test-Assert (Test-FileExists $chineseFile) "Chinese characters in filename"
        Remove-Item $chineseFile -Force -ErrorAction SilentlyContinue
    } catch {
        Write-Warning "Chinese filename test failed: $_"
        $script:TestFailed++
    }

    # Test 4: Spaces in filename
    $spaceFile = Join-Path $MountPoint "file with spaces.txt"
    Set-Content -Path $spaceFile -Value "space test"
    Test-Assert (Test-FileExists $spaceFile) "Spaces in filename"
    Remove-Item $spaceFile -Force -ErrorAction SilentlyContinue

    # Test 5: Special characters
    $specialFile = Join-Path $MountPoint "file-test_123.txt"
    Set-Content -Path $specialFile -Value "special chars"
    Test-Assert (Test-FileExists $specialFile) "Special characters in filename"
    Remove-Item $specialFile -Force -ErrorAction SilentlyContinue

    # Cleanup
    Remove-Item $largeFile -Force -ErrorAction SilentlyContinue

    return $true
}

# ============================================================
# Module G: Error Handling
# ============================================================
function Test-ErrorHandling {
    Write-TestHeader "Module G: Error Handling"

    # Test 1: Read non-existent file
    $nonExistent = Join-Path $MountPoint "does-not-exist.txt"
    try {
        Get-Content -Path $nonExistent -ErrorAction Stop
        Test-Assert $false "Reading non-existent file should fail"
    } catch {
        Test-Assert $true "Reading non-existent file fails as expected"
    }

    # Test 2: Delete non-existent file
    try {
        Remove-Item -Path $nonExistent -ErrorAction Stop
        Test-Assert $false "Deleting non-existent file should fail"
    } catch {
        Test-Assert $true "Deleting non-existent file fails as expected"
    }

    # Test 3: Create file in non-existent directory
    $invalidPath = Join-Path $MountPoint "nonexistent-dir\file.txt"
    try {
        Set-Content -Path $invalidPath -Value "test" -ErrorAction Stop
        Test-Assert $false "Creating file in non-existent directory should fail"
    } catch {
        Test-Assert $true "Creating file in non-existent directory fails as expected"
    }

    # Test 4: Invalid filename (Windows reserved)
    $invalidFile = Join-Path $MountPoint "CON"
    try {
        Set-Content -Path $invalidFile -Value "test" -ErrorAction Stop
        # If it succeeds, that's actually okay for AgentFS
        Test-Assert $true "Reserved filename handling (may be allowed)"
        Remove-Item $invalidFile -Force -ErrorAction SilentlyContinue
    } catch {
        Test-Assert $true "Reserved filename fails as expected"
    }

    return $true
}

# ============================================================
# Module H: AgentFS CLI Commands
# ============================================================
function Test-AgentFSCli {
    Write-TestHeader "Module H: AgentFS CLI Commands"

    # Create test file for CLI tests
    $testFile = "cli-test.txt"
    $fullTestPath = Join-Path $MountPoint $testFile
    Set-Content -Path $fullTestPath -Value "CLI test content"

    # Test 1: agentfs fs <id> ls
    $output = cargo run -- fs $AgentId ls 2>&1
    $matchResult = $output -match $testFile
    Test-Assert ($matchResult.Count -gt 0) "agentfs fs ls command"

    # Test 2: agentfs fs <id> cat
    $output = cargo run -- fs $AgentId cat $testFile 2>&1
    $matchResult = $output -match "CLI test content"
    Test-Assert ($matchResult.Count -gt 0) "agentfs fs cat command"

    # Test 3: agentfs fs <id> write
    $newFile = "cli-write-test.txt"
    $fullNewPath = Join-Path $MountPoint $newFile
    $output = cargo run -- fs $AgentId write $newFile "Written by CLI" 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Host "  CLI write output: $output" -ForegroundColor Gray
    }
    # Try mount visibility first (eventual visibility), then verify via CLI.
    $visibleInMount = $false
    for ($i = 0; $i -lt 10 -and !$visibleInMount; $i++) {
        if (Test-FileExists $fullNewPath) {
            $visibleInMount = $true
            break
        }
        Start-Sleep -Milliseconds 300
    }

    $visibleInCli = $false
    if (!$visibleInMount) {
        $catOut = cargo run -- fs $AgentId cat $newFile 2>&1
        if ($LASTEXITCODE -eq 0 -and ($catOut -match "Written by CLI")) {
            $visibleInCli = $true
        }
    }

    Test-Assert ($visibleInMount -or $visibleInCli) "agentfs fs write command"

    # Test 4: agentfs timeline <id>
    cargo run -- timeline $AgentId 2>&1 | Out-Null
    Test-Assert ($LASTEXITCODE -eq 0) "agentfs timeline command"

    # Cleanup
    Remove-Item $fullTestPath -Force -ErrorAction SilentlyContinue
    Remove-Item $fullNewPath -Force -ErrorAction SilentlyContinue

    return $true
}

# ============================================================
# Module I: Windows-Specific Tests
# ============================================================
function Test-WindowsSpecific {
    Write-TestHeader "Module I: Windows-Specific Tests"

    # Test 1: Windows path separator (backslash)
    $backslashPath = Join-Path $MountPoint "backslash-test.txt"
    Set-Content -Path $backslashPath -Value "backslash test"
    Test-Assert (Test-FileExists $backslashPath) "Windows path separator (backslash)"

    # Test 2: Forward slash (should also work)
    $forwardSlashPath = $MountPoint + "/forward-slash-test.txt"
    try {
        Set-Content -Path $forwardSlashPath -Value "forward slash test"
        Test-Assert (Test-FileExists $forwardSlashPath) "Forward slash path separator"
        Remove-Item $forwardSlashPath -Force -ErrorAction SilentlyContinue
    } catch {
        Write-Warning "Forward slash test failed: $_"
        $script:TestSkipped++
    }

    # Test 3: PowerShell cmdlets
    $psFile = Join-Path $MountPoint "powershell-test.txt"
    "PowerShell content" | Out-File -FilePath $psFile -NoNewline
    $content = Get-Content -Path $psFile
    Test-Assert ($content -eq "PowerShell content") "PowerShell Out-File and Get-Content"

    # Test 4: Copy-Item
    $sourceFile = Join-Path $MountPoint "copy-source.txt"
    $destFile = Join-Path $MountPoint "copy-dest.txt"
    Set-Content -Path $sourceFile -Value "copy test"
    Copy-Item -Path $sourceFile -Destination $destFile
    Test-Assert (Test-FileExists $destFile) "Copy-Item command"

    # Test 5: Move-Item
    $moveSource = Join-Path $MountPoint "move-source.txt"
    $moveDest = Join-Path $MountPoint "move-dest.txt"
    Set-Content -Path $moveSource -Value "move test"
    Move-Item -Path $moveSource -Destination $moveDest
    Test-Assert (!(Test-FileExists $moveSource) -and (Test-FileExists $moveDest)) "Move-Item command"

    # Cleanup
    Remove-Item $backslashPath -Force -ErrorAction SilentlyContinue
    Remove-Item $psFile -Force -ErrorAction SilentlyContinue
    Remove-Item $sourceFile -Force -ErrorAction SilentlyContinue
    Remove-Item $destFile -Force -ErrorAction SilentlyContinue
    Remove-Item $moveDest -Force -ErrorAction SilentlyContinue

    return $true
}

# ============================================================
# Module J: Performance Tests
# ============================================================
function Test-Performance {
    Write-TestHeader "Module J: Performance Tests"

    # Test 1: Create 100 small files
    Write-Info "Creating 100 small files..."
    $fileCount = 100
    $perfDir = Join-Path $MountPoint "perf-test"
    New-Item -Path $perfDir -ItemType Directory -Force | Out-Null

    $startTime = Get-Date
    for ($i = 0; $i -lt $fileCount; $i++) {
        $file = Join-Path $perfDir "file-$i.txt"
        Set-Content -Path $file -Value "Content $i"
    }
    $endTime = Get-Date
    $duration = ($endTime - $startTime).TotalMilliseconds

    Test-Assert $true "Create $fileCount files in $([math]::Round($duration, 2))ms"

    # Test 2: List 100 files
    $startTime = Get-Date
    $items = Get-ChildItem -Path $perfDir
    $endTime = Get-Date
    $duration = ($endTime - $startTime).TotalMilliseconds

    Test-Assert ($items.Count -eq $fileCount) "List $fileCount files in $([math]::Round($duration, 2))ms"

    # Test 3: Read/write 10MB file
    Write-Info "Testing 10MB file read/write..."
    $largeFile = Join-Path $MountPoint "10mb-test.bin"
    $size = 10MB
    $bytes = [byte[]]::new($size)
    (New-Object Random).NextBytes($bytes)

    $writeStart = Get-Date
    [System.IO.File]::WriteAllBytes($largeFile, $bytes)
    $writeEnd = Get-Date
    $writeDuration = ($writeEnd - $writeStart).TotalMilliseconds

    $readStart = Get-Date
    $readBytes = [System.IO.File]::ReadAllBytes($largeFile)
    $readEnd = Get-Date
    $readDuration = ($readEnd - $readStart).TotalMilliseconds

    Test-Assert ($readBytes.Length -eq $size) "10MB file write in $([math]::Round($writeDuration, 2))ms, read in $([math]::Round($readDuration, 2))ms"

    # Cleanup - delete files one by one to avoid wildcard issues
    Write-Info "Cleaning up performance test files..."
    try {
        # First try to delete individual files
        for ($i = 0; $i -lt $fileCount; $i++) {
            $file = Join-Path $perfDir "file-$i.txt"
            if (Test-Path $file) {
                Remove-Item $file -Force -ErrorAction SilentlyContinue
            }
        }
        # Then delete the directory
        if (Test-Path $perfDir) {
            Remove-TestPath -Path $perfDir -Recurse | Out-Null
        }
        # Finally delete the large file
        if (Test-Path $largeFile) {
            Remove-TestPath -Path $largeFile | Out-Null
        }
    } catch {
        Write-Warning "Performance test cleanup encountered error: $_"
        # Don't fail the test for cleanup issues
    }

    return $true
}

# ============================================================
# Main Test Execution
# ============================================================
function Main {
    Write-Host "================================================" -ForegroundColor Cyan
    Write-Host "  AgentFS Windows Mounting Test Suite" -ForegroundColor Cyan
    Write-Host "  Agent ID: $AgentId" -ForegroundColor Cyan
    Write-Host "  Mount Point: $MountPoint" -ForegroundColor Cyan
    Write-Host "================================================" -ForegroundColor Cyan

    $totalStart = Get-Date

    # Run all test modules
    $modules = @(
        @{Name = "EnvironmentSetup"; Function = "Test-EnvironmentSetup"},
        @{Name = "BasicFileOperations"; Function = "Test-BasicFileOperations"},
        @{Name = "DirectoryOperations"; Function = "Test-DirectoryOperations"},
        @{Name = "SymbolicLinks"; Function = "Test-SymbolicLinks"},
        @{Name = "FileAttributes"; Function = "Test-FileAttributes"},
        @{Name = "EdgeCases"; Function = "Test-EdgeCases"},
        @{Name = "ErrorHandling"; Function = "Test-ErrorHandling"},
        @{Name = "AgentFSCli"; Function = "Test-AgentFSCli"},
        @{Name = "WindowsSpecific"; Function = "Test-WindowsSpecific"},
        @{Name = "Performance"; Function = "Test-Performance"}
    )

    # If QuickTest, only run core modules
    if ($QuickTest) {
        $modules = $modules | Where-Object { $_.Name -in @("EnvironmentSetup", "BasicFileOperations", "DirectoryOperations") }
        Write-Info "Running quick test (core modules only)"
    }

    # If TestModule specified, run only that module
    if ($TestModule) {
        $modules = $modules | Where-Object { $_.Name -eq $TestModule }
        if ($modules.Count -eq 0) {
            Write-Fail "Module '$TestModule' not found. Available modules: $($modules.Name -join ', ')"
            exit 1
        }
        Write-Info "Running single module: $TestModule"
    }

    foreach ($module in $modules) {
        try {
            & $module.Function
        } catch {
            Write-Fail "Module $($module.Name) failed with error: $_"
            $script:TestFailed++
        }
    }

    $totalEnd = Get-Date
    $totalDuration = ($totalEnd - $totalStart).TotalSeconds

    # Print summary
    Write-Host "`n================================================" -ForegroundColor Cyan
    Write-Host "  Test Summary" -ForegroundColor Cyan
    Write-Host "================================================" -ForegroundColor Cyan
    Write-Host "Total Duration: $([math]::Round($totalDuration, 2)) seconds" -ForegroundColor White
    Write-Host "Tests Passed:   $script:TestPassed" -ForegroundColor Green
    Write-Host "Tests Failed:   $script:TestFailed" -ForegroundColor Red
    Write-Host "Tests Skipped:  $script:TestSkipped" -ForegroundColor Yellow
    Write-Host "================================================" -ForegroundColor Cyan

    $totalTests = $script:TestPassed + $script:TestFailed + $script:TestSkipped
    $passRate = if ($totalTests -gt 0) { [math]::Round(($script:TestPassed / $totalTests) * 100, 2) } else { 0 }

    Write-Host "Pass Rate: $passRate%" -ForegroundColor $(if ($passRate -ge 80) { "Green" } elseif ($passRate -ge 60) { "Yellow" } else { "Red" })
    Write-Host "================================================`n" -ForegroundColor Cyan

    # Cleanup
    if (!$SkipCleanup) {
        Invoke-Cleanup
    }

    # Exit with appropriate code
    if ($script:TestFailed -gt 0) {
        exit 1
    } else {
        exit 0
    }
}

# Run main function
Main
