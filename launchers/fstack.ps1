$ErrorActionPreference = 'Stop'
$installRuntime = $args.Count -eq 1 -and $args[0] -ceq '-InstallRuntime'
$runtimePath = $args.Count -eq 1 -and $args[0] -ceq '-RuntimePath'
$package = if ($PSScriptRoot) { Split-Path -Parent $PSScriptRoot } else { $env:PLUGIN_ROOT }
if (!$package) { throw 'fstack: package path is missing' }

function Test-RuntimeFile([string] $Path) {
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    return ($item -and !$item.PSIsContainer -and !($item.Attributes -band [IO.FileAttributes]::ReparsePoint))
}

$binary = Join-Path $package 'bin\fstack.exe'
if (!(Test-RuntimeFile $binary)) {
    $architecture = $env:PROCESSOR_ARCHITEW6432
    if (!$architecture) { $architecture = $env:PROCESSOR_ARCHITECTURE }
    if ($architecture -ne 'AMD64') { throw 'fstack: supported Windows architecture is x64' }

    $manifest = Join-Path $package '.codex-plugin\plugin.json'
    $version = ([IO.File]::ReadAllText($manifest) | ConvertFrom-Json).version
    if ($version -cnotmatch '\A[0-9]+\.[0-9]+\.[0-9]+\z') { throw 'fstack: invalid package version' }
    $target = 'x86_64-pc-windows-msvc'
    $codexHome = $env:CODEX_HOME
    if (!$codexHome) { $codexHome = Join-Path $env:USERPROFILE '.codex' }
    $directory = [IO.Path]::GetFullPath((Join-Path $codexHome "fstack\runtime\$version\$target"))
    $binary = Join-Path $directory 'fstack.exe'
    if ($InstallRuntime -and !(Test-RuntimeFile $binary)) {
        $asset = "fstack-$version-$target.exe"
        $url = "https://github.com/WhyFaust/fstack/releases/download/v$version/$asset"
        $curl = Join-Path $env:SystemRoot 'System32\curl.exe'
        $staging = Join-Path $directory ('.download-' + [Guid]::NewGuid().ToString('N'))
        [void][IO.Directory]::CreateDirectory($staging)
        try {
            foreach ($suffix in @('.sha256', '')) {
                $download = Join-Path $staging "$asset$suffix"
                & $curl -q --proto '=https' --proto-redir '=https' --fail --silent --show-error --location --connect-timeout 15 --max-time 180 --retry 2 --retry-max-time 180 "$url$suffix" --output $download
                if ($LASTEXITCODE -ne 0) { throw "fstack: download failed ($LASTEXITCODE)" }
            }
            $download = Join-Path $staging $asset
            $digest = (Get-FileHash -LiteralPath $download -Algorithm SHA256).Hash.ToLowerInvariant()
            $checksum = [IO.File]::ReadAllText((Join-Path $staging "$asset.sha256")).TrimEnd("`r", "`n")
            if ($checksum -cne "$digest  $asset" -or (Get-Item -LiteralPath $download).Length -eq 0) {
                throw 'fstack: runtime checksum mismatch'
            }
            try {
                [IO.File]::Move($download, $binary)
            } catch [IO.IOException] {
                if (!(Test-RuntimeFile $binary) -or (Get-FileHash -LiteralPath $binary).Hash.ToLowerInvariant() -cne $digest) { throw }
            }
        } finally {
            Remove-Item -LiteralPath $staging -Recurse -Force
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
