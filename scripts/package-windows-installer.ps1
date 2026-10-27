# Compile a per-user development installer from an already built bundle.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$BundleDirectory,
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,
    [string]$CompilerPath
)

$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'Run installer packaging on Windows.' }
$repo = Split-Path $PSScriptRoot -Parent
$bundle = [IO.Path]::GetFullPath($BundleDirectory).TrimEnd('\')
$output = [IO.Path]::GetFullPath($OutputDirectory).TrimEnd('\')
foreach ($path in @($repo, $bundle, $output)) {
    if ($path -match '[\x00-\x1f"{}]' -or $path -match '^[A-Za-z]:$') {
        throw 'Installer paths must be non-root paths without controls, quotes or braces.'
    }
}
foreach ($parent in @($bundle, $repo)) {
    if ($output.Equals($parent, [StringComparison]::OrdinalIgnoreCase) -or
        $output.StartsWith($parent + '\', [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Keep installer output outside the source and bundle directories.'
    }
}

$expected = @('sift-desktop.exe', 'sift-server.exe', 'sift-launcher.exe', 'sift-admin.exe', 'sift-remote.exe', 'sift.exe', 'LICENSE', 'README.txt', 'source.tar', 'licenses/LICENSE.qlementine-icons', 'licenses/tiberius-APACHE.txt', 'licenses/tiberius-MIT.txt')
foreach ($directory in @($bundle, (Join-Path $bundle 'licenses'))) {
    $item = Get-Item -LiteralPath $directory -Force
    if (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw 'Bundle directories must be ordinary directories.'
    }
}
$manifestPath = Join-Path $bundle 'bundle.json'
$manifestFile = Get-Item -LiteralPath $manifestPath -Force
if ($manifestFile.PSIsContainer -or $manifestFile.Length -gt 131072 -or
    ($manifestFile.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
    throw 'Invalid bundle manifest file.'
}
$manifestHash = (Get-FileHash -LiteralPath $manifestPath -Algorithm SHA256).Hash
$manifest = [IO.File]::ReadAllText($manifestPath) | ConvertFrom-Json
if ((Get-FileHash -LiteralPath $manifestPath -Algorithm SHA256).Hash -ne $manifestHash) {
    throw 'Bundle manifest changed while reading it.'
}
if ($manifest.schema_version -ne 1 -or $manifest.product -ne 'Sift' -or
    $manifest.target -ne 'x86_64-pc-windows-msvc' -or
    $manifest.profile -notin @('dev', 'release-dev', 'release') -or
    $manifest.source_tree -isnot [string] -or $manifest.version -isnot [string] -or
    $manifest.source_tree -notmatch '\A[0-9a-f]{40}\z' -or
    $manifest.version -notmatch '\A[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?\z' -or
    @($manifest.files).Count -ne $expected.Count) {
    throw 'Unsupported bundle manifest.'
}
$seen = @{}
foreach ($file in $manifest.files) {
    if ($file.path -isnot [string] -or $file.sha256 -isnot [string] -or
        ($file.bytes -isnot [int] -and $file.bytes -isnot [long]) -or $file.bytes -le 0 -or
        $file.path -notin $expected -or $seen.ContainsKey($file.path) -or
        $file.sha256 -notmatch '\A[0-9a-f]{64}\z') {
        throw 'Invalid or duplicate payload path/hash.'
    }
    $seen[$file.path] = $true
    $item = Get-Item -LiteralPath (Join-Path $bundle $file.path) -Force
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
        $item.Length -ne $file.bytes -or
        (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash -ne $file.sha256) {
        throw ('Bundle payload mismatch: ' + $file.path)
    }
}
# Enumerate the fixed directory shape without following unknown subdirectories.
$rootNames = @($expected | Where-Object { $_ -notlike '*/*' }) + @('bundle.json', 'licenses')
if (@(Get-ChildItem -LiteralPath $bundle -Force | Where-Object { $_.Name -notin $rootNames }).Count -or
    @(Get-ChildItem -LiteralPath (Join-Path $bundle 'licenses') -Force).Count -ne 3) {
    throw 'Unexpected bundle content.'
}
if (-not $CompilerPath) {
    $candidates = @((Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6/ISCC.exe'), (Join-Path $env:LOCALAPPDATA 'Programs/Inno Setup 6/ISCC.exe'))
    $CompilerPath = $candidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
}
if (-not $CompilerPath) { throw 'Install Inno Setup 6 or supply -CompilerPath.' }
$CompilerPath = (Get-Item -LiteralPath $CompilerPath).FullName
$recipe = Join-Path $repo 'packaging/windows/sift-development.iss'
$name = "sift-$($manifest.version)-windows-x64-development-setup-$($manifest.source_tree.Substring(0,12))"
$installer = Join-Path $output ($name + '.exe')
if ((Test-Path -LiteralPath $installer) -or (Test-Path -LiteralPath "$installer.json") -or
    (Test-Path -LiteralPath "$installer.sha256")) { throw 'Installer output already exists.' }
[IO.Directory]::CreateDirectory($output) | Out-Null
$staging = Join-Path ([IO.Path]::GetTempPath()) ('sift-installer-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($staging) | Out-Null
try {
    $stagedBundle = Join-Path $staging 'bundle'
    [IO.Directory]::CreateDirectory((Join-Path $stagedBundle 'licenses')) | Out-Null
    Copy-Item -LiteralPath $manifestPath -Destination (Join-Path $stagedBundle 'bundle.json')
    if ((Get-FileHash -LiteralPath (Join-Path $stagedBundle 'bundle.json') -Algorithm SHA256).Hash -ne $manifestHash) {
        throw 'Bundle manifest changed while staging it.'
    }
    foreach ($file in $manifest.files) {
        $destination = Join-Path $stagedBundle $file.path
        Copy-Item -LiteralPath (Join-Path $bundle $file.path) -Destination $destination
        if ((Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash -ne $file.sha256) {
            throw 'Bundle changed while staging the installer.'
        }
    }
    $versionInfo = [Diagnostics.FileVersionInfo]::GetVersionInfo((Join-Path $stagedBundle 'sift-desktop.exe'))
    if ($versionInfo.ProductName -ne 'Sift' -or $versionInfo.ProductVersion -ne $manifest.version) {
        throw 'Staged desktop metadata does not match the manifest.'
    }
    $stagedRecipe = Join-Path $staging 'recipe.iss'
    Copy-Item -LiteralPath $recipe -Destination $stagedRecipe
    $iconDirectory = Join-Path $staging 'crates/desktop/assets'
    [IO.Directory]::CreateDirectory($iconDirectory) | Out-Null
    $icon = Join-Path $iconDirectory 'sift-icon.ico'
    Copy-Item -LiteralPath (Join-Path $repo 'crates/desktop/assets/sift-icon.ico') -Destination $icon
    $compiled = Join-Path $staging 'compiled'
    $compilerHash = (Get-FileHash -LiteralPath $CompilerPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $compileArguments = @('/Qp', "/DBundleDirectory=$stagedBundle", "/DInstallerOutputDirectory=$compiled", "/DInstallerName=$name", "/DAppVersion=$($manifest.version)", "/DRepositoryDirectory=$staging", $stagedRecipe)
    $ErrorActionPreference = 'Continue'
    & $CompilerPath @compileArguments
    if ($LASTEXITCODE -ne 0) { throw "Installer compiler failed (exit $LASTEXITCODE)." }
    $ErrorActionPreference = 'Stop'
    if ((Get-FileHash -LiteralPath $CompilerPath -Algorithm SHA256).Hash -ne $compilerHash) {
        throw 'Installer compiler changed during compilation.'
    }
    $compiledInstaller = Join-Path $compiled ($name + '.exe')
    $hash = (Get-FileHash -LiteralPath $compiledInstaller -Algorithm SHA256).Hash.ToLowerInvariant()
    $receipt = [ordered]@{
        schema_version = 1
        channel = 'development'
        installer = [IO.Path]::GetFileName($installer)
        sha256 = $hash
        source_tree = $manifest.source_tree
        bundle_manifest_sha256 = (Get-FileHash -LiteralPath (Join-Path $stagedBundle 'bundle.json') -Algorithm SHA256).Hash.ToLowerInvariant()
        recipe_sha256 = (Get-FileHash -LiteralPath $stagedRecipe -Algorithm SHA256).Hash.ToLowerInvariant()
        icon_sha256 = (Get-FileHash -LiteralPath $icon -Algorithm SHA256).Hash.ToLowerInvariant()
        compiler_sha256 = $compilerHash
        authenticode = [string](Get-AuthenticodeSignature -LiteralPath $compiledInstaller).Status
    }
    Move-Item -LiteralPath $compiledInstaller -Destination $installer
    [IO.File]::WriteAllText("$installer.json", ($receipt | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
    [IO.File]::WriteAllText("$installer.sha256", "$hash  $([IO.Path]::GetFileName($installer))`n", [Text.UTF8Encoding]::new($false))
    Write-Output $installer
} finally {
    Remove-Item -LiteralPath $staging -Recurse -Force
}
