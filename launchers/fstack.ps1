$ErrorActionPreference = 'Stop'
$installRuntime = $args.Count -eq 1 -and $args[0] -ceq '-InstallRuntime'
$runtimePath = $args.Count -eq 1 -and $args[0] -ceq '-RuntimePath'
$package = if ($PSScriptRoot) { [IO.Path]::GetDirectoryName($PSScriptRoot) } else { $env:PLUGIN_ROOT }
if (!$package) { throw 'fstack: package path is missing' }

function Test-RuntimeFile([string] $Path) {
    if (![IO.File]::Exists($Path)) { return $false }
    return !([IO.File]::GetAttributes($Path) -band [IO.FileAttributes]::ReparsePoint)
}

function Get-RuntimeHash([string] $Path) {
    $stream = [IO.File]::OpenRead($Path)
    try {
        return (Get-FileHash -InputStream $stream -Algorithm SHA256).Hash.ToLowerInvariant()
    } finally {
        $stream.Dispose()
    }
}

$binary = [IO.Path]::Combine($package, 'bin', 'fstack.exe')
if (!(Test-RuntimeFile $binary)) {
    $architecture = $env:PROCESSOR_ARCHITEW6432
    if (!$architecture) { $architecture = $env:PROCESSOR_ARCHITECTURE }
    if ($architecture -ne 'AMD64') { throw 'fstack: supported Windows architecture is x64' }

    $manifest = [IO.Path]::Combine($package, '.codex-plugin', 'plugin.json')
    $version = ([IO.File]::ReadAllText($manifest) | ConvertFrom-Json).version
    if ($version -cnotmatch '\A[0-9]+\.[0-9]+\.[0-9]+\z') { throw 'fstack: invalid package version' }
    $target = 'x86_64-pc-windows-msvc'
    $codexHome = $env:CODEX_HOME
    if (!$codexHome) { $codexHome = [IO.Path]::Combine($env:USERPROFILE, '.codex') }
    $directory = [IO.Path]::GetFullPath([IO.Path]::Combine($codexHome, "fstack\runtime\$version\$target"))
    $binary = [IO.Path]::Combine($directory, 'fstack.exe')
    if ($InstallRuntime -and !(Test-RuntimeFile $binary)) {
        $asset = "fstack-$version-$target.exe"
        $url = "https://github.com/WhyFaust/fstack/releases/download/v$version/$asset"
        $curl = [IO.Path]::Combine($env:SystemRoot, 'System32', 'curl.exe')
        $staging = [IO.Path]::Combine($directory, '.download-' + [Guid]::NewGuid().ToString('N'))
        [void][IO.Directory]::CreateDirectory($staging)
        try {
            foreach ($suffix in @('.sha256', '')) {
                $download = [IO.Path]::Combine($staging, "$asset$suffix")
                & $curl -q --proto '=https' --proto-redir '=https' --fail --silent --show-error --location --connect-timeout 15 --max-time 180 --retry 2 --retry-max-time 180 "$url$suffix" --output $download
                if ($LASTEXITCODE -ne 0) { throw "fstack: download failed ($LASTEXITCODE)" }
            }
            $download = [IO.Path]::Combine($staging, $asset)
            $digest = Get-RuntimeHash $download
            $checksum = [IO.File]::ReadAllText([IO.Path]::Combine($staging, "$asset.sha256")).TrimEnd("`r", "`n")
            if ($checksum -cne "$digest  $asset" -or ([IO.FileInfo]::new($download)).Length -eq 0) {
                throw 'fstack: runtime checksum mismatch'
            }
            try {
                [IO.File]::Move($download, $binary)
            } catch [IO.IOException] {
                if (!(Test-RuntimeFile $binary) -or (Get-RuntimeHash $binary) -cne $digest) { throw }
            }
        } finally {
            [IO.Directory]::Delete($staging, $true)
        }
    }
}

if (!(Test-RuntimeFile $binary)) {
    if ($RuntimePath) { return }
    throw 'fstack: open a chat with the trusted hook or invoke this script with -InstallRuntime'
}
if ($InstallRuntime -or $RuntimePath) {
    $binary
    return
}
& $binary --package $package @args
exit $LASTEXITCODE
