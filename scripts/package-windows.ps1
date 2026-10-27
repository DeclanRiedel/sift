# Build a native portable bundle from a clean source checkout.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,
    [ValidateSet('dev', 'release', 'release-dev')]
    [string]$BuildProfile = 'release'
)

$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'Run packaging on a native Windows host.' }
$repo = Split-Path $PSScriptRoot -Parent

function Invoke-Native {
    param([string]$Program, [string[]]$CommandArguments)
    $ErrorActionPreference = 'Continue'
    $result = & $Program @CommandArguments
    if ($LASTEXITCODE -ne 0) { throw "$Program failed (exit $LASTEXITCODE)." }
    return $result
}

Push-Location $repo
try {
    $gitRoot = [IO.Path]::GetFullPath((Invoke-Native 'git' @('rev-parse', '--show-toplevel')).Trim())
    if (-not $gitRoot.Equals([IO.Path]::GetFullPath($repo), [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Package from the Sift repository root.'
    }
    if (Invoke-Native 'git' @('ls-files', '--', '.env', '.sift', '.cache', 'target')) {
        throw 'Runtime or build data is tracked; refusing to archive it.'
    }
    if (Invoke-Native 'git' @('status', '--porcelain', '--untracked-files=normal')) {
        throw 'Packaging requires a clean source checkout.'
    }
    $sourceTree = (Invoke-Native 'git' @('rev-parse', 'HEAD^{tree}')).Trim()
    $sourceCommit = (Invoke-Native 'git' @('rev-parse', 'HEAD')).Trim()
    $compiler = (Invoke-Native 'rustc' @('--version')).Trim()
    $hostInfo = Invoke-Native 'rustc' @('-vV')
    if (-not ($hostInfo -contains 'host: x86_64-pc-windows-msvc')) {
        throw 'This bundle currently supports native x64 MSVC builds only.'
    }
    $metadata = (Invoke-Native 'cargo' @('metadata', '--format-version', '1', '--no-deps', '--locked') | Out-String) | ConvertFrom-Json
    $desktop = @($metadata.packages | Where-Object { $_.name -eq 'sift-desktop' })
    if ($desktop.Count -ne 1) { throw 'Cannot identify the desktop package.' }
    $version = $desktop[0].version
    $output = [IO.Path]::GetFullPath($OutputDirectory)
    if ($output.StartsWith($repo.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase) -or $output -eq $repo) {
        throw 'Keep packaging output outside the source checkout.'
    }
    [IO.Directory]::CreateDirectory($output) | Out-Null
    $name = "sift-$version-windows-x64-$BuildProfile-$($sourceTree.Substring(0,12))"
    $bundle = Join-Path $output $name
    $archive = "$bundle.zip"
    $partialArchive = "$archive.partial"
    if ((Test-Path -LiteralPath $bundle) -or (Test-Path -LiteralPath $archive) -or (Test-Path -LiteralPath $partialArchive)) {
        throw 'Bundle output already exists; choose a new output directory.'
    }
    Invoke-Native 'cargo' @('build', '--locked', '--target', 'x86_64-pc-windows-msvc', '-p', 'sift-desktop', '-p', 'sift-server', '--bins', '--profile', $BuildProfile) | Out-Host
    if ((Invoke-Native 'git' @('status', '--porcelain', '--untracked-files=normal')) -or
        (Invoke-Native 'git' @('rev-parse', 'HEAD')).Trim() -ne $sourceCommit) {
        throw 'Source changed during the build; refusing to package.'
    }
    $buildDirectory = Join-Path (Join-Path $metadata.target_directory 'x86_64-pc-windows-msvc') $(if ($BuildProfile -eq 'dev') { 'debug' } else { $BuildProfile })
    $versionInfo = [Diagnostics.FileVersionInfo]::GetVersionInfo((Join-Path $buildDirectory 'sift-desktop.exe'))
    if ($versionInfo.ProductName -ne 'Sift' -or $versionInfo.ProductVersion -ne $version) {
        throw 'Desktop executable metadata does not match the source version.'
    }
    [IO.Directory]::CreateDirectory($bundle) | Out-Null
    $incomplete = Join-Path $bundle '.incomplete'
    [IO.File]::WriteAllText($incomplete, 'Packaging in progress')
    $executables = @('sift-desktop.exe', 'sift-server.exe', 'sift-launcher.exe', 'sift-admin.exe', 'sift-remote.exe', 'sift.exe')
    foreach ($executable in $executables) {
        Copy-Item -LiteralPath (Join-Path $buildDirectory $executable) -Destination $bundle
    }
    Copy-Item -LiteralPath (Join-Path $repo 'LICENSE') -Destination $bundle
    $licenses = Join-Path $bundle 'licenses'
    [IO.Directory]::CreateDirectory($licenses) | Out-Null
    Copy-Item -LiteralPath (Join-Path $repo 'crates/ui/assets/icons/LICENSE.qlementine-icons') -Destination $licenses
    Copy-Item -LiteralPath (Join-Path $repo 'vendor/tiberius/LICENSE-APACHE.txt') -Destination (Join-Path $licenses 'tiberius-APACHE.txt')
    Copy-Item -LiteralPath (Join-Path $repo 'vendor/tiberius/LICENSE-MIT.txt') -Destination (Join-Path $licenses 'tiberius-MIT.txt')
    $readme = @"
Sift $version for Windows x64

Launch sift-desktop.exe. Keep all six executables together: the desktop uses
its sibling launcher and server for local instances. The CLI is sift.exe.
Install a compatible Microsoft Visual C++ v14 x64 Redistributable first.
Windows supplies ICU and Direct3D system libraries used by the desktop.

bundle.json records the build profile, source identity, file hashes and native
signature status. source.tar contains the corresponding clean source snapshot.
This bundle contains no development environment file or user runtime data.
"@
    [IO.File]::WriteAllText((Join-Path $bundle 'README.txt'), $readme, [Text.UTF8Encoding]::new($false))
    Invoke-Native 'git' @('archive', '--format=tar', '--output', (Join-Path $bundle 'source.tar'), $sourceCommit) | Out-Host
    $files = @(Get-ChildItem -LiteralPath $bundle -File -Recurse | Where-Object { $_.Name -ne '.incomplete' } | Sort-Object FullName | ForEach-Object {
        $relative = $_.FullName.Substring($bundle.Length + 1).Replace('\', '/')
        $signature = if ($_.Extension -eq '.exe') { [string](Get-AuthenticodeSignature -LiteralPath $_.FullName).Status } else { $null }
        [ordered]@{ path = $relative; bytes = $_.Length; sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(); authenticode = $signature }
    })
    $manifest = [ordered]@{
        schema_version = 1
        product = 'Sift'
        version = $version
        target = 'x86_64-pc-windows-msvc'
        profile = $BuildProfile
        compiler = $compiler
        source_commit = $sourceCommit
        source_tree = $sourceTree
        runtime_prerequisites = @('Microsoft Visual C++ v14 x64 Redistributable', 'Windows ICU and Direct3D system libraries')
        created_at = [DateTime]::UtcNow.ToString('o')
        files = $files
    }
    $manifestPath = Join-Path $bundle 'bundle.json'
    [IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
    Remove-Item -LiteralPath $incomplete
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [IO.Compression.ZipFile]::CreateFromDirectory($bundle, $partialArchive, [IO.Compression.CompressionLevel]::Optimal, $true)
    $hash = (Get-FileHash -LiteralPath $partialArchive -Algorithm SHA256).Hash.ToLowerInvariant()
    Move-Item -LiteralPath $partialArchive -Destination $archive
    [IO.File]::WriteAllText("$archive.sha256", "$hash  $([IO.Path]::GetFileName($archive))`n", [Text.UTF8Encoding]::new($false))
    Write-Output $archive
} finally {
    Pop-Location
}
