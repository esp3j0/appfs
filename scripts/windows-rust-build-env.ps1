function Resolve-MsvcToolsInstallDir {
    if ($env:VCToolsInstallDir) {
        $vcToolsInstallDir = $env:VCToolsInstallDir.TrimEnd("\/")
        if (Test-Path (Join-Path $vcToolsInstallDir "include") -PathType Container) {
            return $vcToolsInstallDir
        }
    }

    $cl = Get-Command cl.exe -ErrorAction SilentlyContinue
    if ($null -ne $cl) {
        $clDir = Split-Path $cl.Source -Parent
        $vcToolsInstallDir = $clDir
        1..3 | ForEach-Object {
            $vcToolsInstallDir = Split-Path $vcToolsInstallDir -Parent
        }

        if (Test-Path (Join-Path $vcToolsInstallDir "include") -PathType Container) {
            return $vcToolsInstallDir
        }
    }

    $vswhere = Get-Command vswhere.exe -ErrorAction SilentlyContinue
    if ($null -eq $vswhere) {
        $vswhereDefaultPath = "C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe"
        if (Test-Path $vswhereDefaultPath -PathType Leaf) {
            $vswhere = Get-Item $vswhereDefaultPath
        }
    }

    if ($null -ne $vswhere) {
        $vswherePath = if ($vswhere.PSObject.Properties.Name -contains "Source") {
            $vswhere.Source
        } else {
            $vswhere.FullName
        }

        $installationPath = & $vswherePath `
            -latest `
            -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
            -property installationPath
        if ($LASTEXITCODE -eq 0 -and -not [string]::IsNullOrWhiteSpace($installationPath)) {
            $msvcRoot = Join-Path $installationPath.Trim() "VC\Tools\MSVC"
            $latestMsvcDir = Get-ChildItem -Path $msvcRoot -Directory -ErrorAction SilentlyContinue |
                Sort-Object Name -Descending |
                Select-Object -First 1 -ExpandProperty FullName
            if (-not [string]::IsNullOrWhiteSpace($latestMsvcDir)) {
                return $latestMsvcDir
            }
        }
    }

    throw "Unable to locate MSVC tools include directory. Install Visual Studio Build Tools or add cl.exe to PATH."
}

function Initialize-WindowsRustBuildEnv {
    $vcToolsInstallDir = Resolve-MsvcToolsInstallDir
    $vcInclude = Join-Path $vcToolsInstallDir "include"
    if (!(Test-Path $vcInclude -PathType Container)) {
        throw "MSVC include directory not found: $vcInclude"
    }

    $sdkIncludeRoot = "C:\Program Files (x86)\Windows Kits\10\Include"
    $sdkIncludeDir = Get-ChildItem -Path $sdkIncludeRoot -Directory -ErrorAction Stop |
        Sort-Object Name -Descending |
        Select-Object -First 1 -ExpandProperty FullName

    if ([string]::IsNullOrWhiteSpace($sdkIncludeDir)) {
        throw "Windows SDK include directory not found under $sdkIncludeRoot"
    }

    $includeDirs = @(
        $vcInclude,
        (Join-Path $sdkIncludeDir "ucrt"),
        (Join-Path $sdkIncludeDir "shared"),
        (Join-Path $sdkIncludeDir "um"),
        (Join-Path $sdkIncludeDir "winrt"),
        (Join-Path $sdkIncludeDir "cppwinrt")
    ) | Where-Object { Test-Path $_ -PathType Container }

    if ($includeDirs.Count -eq 0) {
        throw "Unable to resolve Windows SDK include directories for bindgen"
    }

    $clangArgs = ($includeDirs | ForEach-Object { '-isystem"{0}"' -f $_ }) -join " "
    $env:BINDGEN_EXTRA_CLANG_ARGS = $clangArgs
    $env:BINDGEN_EXTRA_CLANG_ARGS_x86_64_pc_windows_msvc = $clangArgs
    Set-Item "Env:BINDGEN_EXTRA_CLANG_ARGS_x86_64-pc-windows-msvc" $clangArgs

    if ([string]::IsNullOrWhiteSpace($env:LIBCLANG_PATH)) {
        $defaultLibclangPath = "C:\Program Files\LLVM\bin"
        if (Test-Path $defaultLibclangPath -PathType Container) {
            $env:LIBCLANG_PATH = $defaultLibclangPath
        }
    }
}
