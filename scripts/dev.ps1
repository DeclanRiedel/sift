# Native Windows development. Environment values stay in the ignored root .env.
[CmdletBinding()]
param(
    [ValidateSet('setup', 'env', 'build', 'check', 'test', 'server', 'desktop')]
    [string]$Action = 'env',
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$Arguments = @()
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent

function Invoke-Cargo {
    param([string[]]$CommandArguments)
    # Windows PowerShell treats redirected native stderr as ErrorRecords.
    # Cargo writes normal progress there; its exit code determines failure.
    $ErrorActionPreference = 'Continue'
    & cargo @CommandArguments
    if ($LASTEXITCODE -ne 0) { throw "Cargo failed (exit $LASTEXITCODE)." }
}

function Initialize-DevMetadata {
    if ($env:SIFT_DEPLOYMENT -in @($null, '', 'personal') -and
        $env:SIFT_MODE -in @($null, '', 'in-process') -and $env:SIFT_METADATA__ENABLED -ne 'false') {
        $status = Invoke-Cargo @('run', '--locked', '-p', 'sift-server', '--bin', 'sift-server', '--', 'migrate', 'status') |
            Out-String | ConvertFrom-Json
        if ($status.pending.Count -gt 0) {
            Invoke-Cargo @('run', '--locked', '-p', 'sift-server', '--bin', 'sift-server', '--', 'migrate', 'apply', '--automatic')
        }
    }
}

Push-Location $repo
try {
    if ($Action -eq 'setup') {
        foreach ($tool in @('git', 'rustup', 'cmake')) {
            if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
                throw "Missing $tool. See docs/DEVELOPMENT.md for Windows prerequisites."
            }
        }
        $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
        if (-not (Test-Path $vswhere)) { throw 'Install Visual Studio C++ Build Tools; see docs/DEVELOPMENT.md.' }
        $visualStudio = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        if (-not $visualStudio) { throw 'Install the Desktop development with C++ workload and Windows SDK.' }
        & rustup show active-toolchain
        if ($LASTEXITCODE -ne 0) { throw 'Rust toolchain installation failed.' }
        & rustup component add rustfmt clippy rust-src rust-analyzer
        if ($LASTEXITCODE -ne 0) { throw 'Rust component installation failed.' }
    }

    if (-not (Test-Path -LiteralPath '.env')) {
        $template = [IO.File]::ReadAllText((Join-Path $repo '.env.example'))
        $template = $template.Replace('SIFT_PG_HOST=/tmp/sift-pg-socket', 'SIFT_PG_HOST=127.0.0.1')
        [IO.File]::WriteAllText((Join-Path $repo '.env'), $template, [Text.UTF8Encoding]::new($false))
    }
    # Parse values as data, never as PowerShell. Outer quotes are optional;
    # retain embedded quotes and equals signs used by Figment's TOML values.
    $lineNumber = 0
    foreach ($line in [IO.File]::ReadAllLines((Join-Path $repo '.env'))) {
        $lineNumber++
        if ($line -match '^\s*(#|$)') { continue }
        if ($line -notmatch '^\s*([A-Za-z_][A-Za-z0-9_]*)=(.*)$') {
            throw "Invalid .env assignment on line $lineNumber."
        }
        $name = $Matches[1]
        $value = $Matches[2].Trim()
        if ($value.Length -ge 2 -and (($value.StartsWith('"') -and $value.EndsWith('"')) -or ($value.StartsWith("'") -and $value.EndsWith("'")))) {
            $value = $value.Substring(1, $value.Length - 2)
        }
        [Environment]::SetEnvironmentVariable($name, $value, 'Process')
    }

    if (-not $env:SIFT_METADATA__SECRET_KEY_FILE) {
        $env:SIFT_METADATA__SECRET_KEY_FILE = Join-Path $repo '.sift/dev-secret.key'
    }
    $keyPath = [IO.Path]::GetFullPath($env:SIFT_METADATA__SECRET_KEY_FILE)
    if (-not (Test-Path -LiteralPath $keyPath)) {
        [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($keyPath)) | Out-Null
        $file = [IO.File]::Open($keyPath, [IO.FileMode]::CreateNew)
        $file.Dispose()
        # Restrict access before writing any key bytes.
        $acl = Get-Acl -LiteralPath $keyPath
        $acl.SetAccessRuleProtection($true, $false)
        $identity = [Security.Principal.WindowsIdentity]::GetCurrent().User
        $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($identity, 'FullControl', 'Allow'))
        Set-Acl -LiteralPath $keyPath -AclObject $acl
        $bytes = New-Object byte[] 32
        $random = [Security.Cryptography.RandomNumberGenerator]::Create()
        try { $random.GetBytes($bytes) } finally { $random.Dispose() }
        $hex = [BitConverter]::ToString($bytes).Replace('-', '').ToLowerInvariant()
        [IO.File]::WriteAllText($keyPath, $hex, [Text.UTF8Encoding]::new($false))
    }

    switch ($Action) {
        'setup' { Write-Host 'Windows development environment ready. Run scripts/dev.ps1 desktop or check.' }
        'env' { Write-Host 'Loaded .env for this PowerShell session.' }
        'build' { Invoke-Cargo (@('build', '--workspace', '--locked') + $Arguments) }
        'check' {
            Invoke-Cargo @('fmt', '--all', '--', '--check')
            Invoke-Cargo @('clippy', '--workspace', '--all-targets', '--locked', '--', '-D', 'warnings')
            Invoke-Cargo (@('test', '--workspace', '--locked') + $Arguments)
        }
        'test' { Invoke-Cargo (@('test', '--workspace', '--locked') + $Arguments) }
        'server' {
            if ($Arguments.Count -eq 0) { Initialize-DevMetadata }
            Invoke-Cargo (@('run', '--locked', '-p', 'sift-server', '--bin', 'sift-server', '--') + $Arguments)
        }
        'desktop' {
            # The desktop supervises sibling executables for local instances.
            Invoke-Cargo @('build', '--locked', '-p', 'sift-server', '--bins')
            $externalStartup = $env:SIFT_DESKTOP__SERVER_URL -or $env:SIFT_DESKTOP__INSTANCE_ROOT -or
                ($Arguments | Where-Object { $_ -match '^--(server-url|instance-root)(=|$)' })
            if (-not $externalStartup) { Initialize-DevMetadata }
            Invoke-Cargo (@('run', '--locked', '-p', 'sift-desktop', '--') + $Arguments)
        }
    }
} finally {
    Pop-Location
}
