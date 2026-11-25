<#
.SYNOPSIS
  Puts the bundled ConPTY (conpty.dll + OpenConsole.exe) in .conpty\<arch>\ for the build to
  copy beside the executables. The binaries are never committed.

.DESCRIPTION
  Default: downloads Microsoft.Windows.Console.ConPTY (the version the WPF app ships) from
  nuget.org. -From copies from an existing folder instead (e.g. the WPF app's build output).
  The nupkg must match the pinned SHA-256. Either way both files must carry a valid Microsoft
  Authenticode signature.

.EXAMPLE
  pwsh scripts/get-conpty.ps1
  pwsh scripts/get-conpty.ps1 -From ..\..\CodeShellManager\src\CodeShellManager\bin\Release\net10.0-windows
#>
param(
    [string]$From,
    [ValidateSet('x64', 'arm64', 'x86')][string]$Arch = 'x64',
    [string]$Version = '1.25.260930003',
    # SHA-256 of the nupkg for $Version (nuget.org). Change both together; a different version
    # needs its hash computed from a trusted download.
    [string]$Sha256 = '02b07b349af66d801159bdf9e440d4a1ce78bb951f37fc8609731665afdae7ee'
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root ".conpty\$Arch"
New-Item -ItemType Directory -Force $dest | Out-Null

if ($From) {
    foreach ($f in 'conpty.dll', 'OpenConsole.exe') {
        Copy-Item (Join-Path $From $f) (Join-Path $dest $f) -Force
    }
} else {
    $tmp = Join-Path ([IO.Path]::GetTempPath()) "csm-conpty-$Version"
    New-Item -ItemType Directory -Force $tmp | Out-Null
    $pkg = Join-Path $tmp 'conpty.nupkg'
    Remove-Item $pkg -Force -ErrorAction SilentlyContinue
    Invoke-WebRequest "https://www.nuget.org/api/v2/package/Microsoft.Windows.Console.ConPTY/$Version" -OutFile $pkg
    $actual = (Get-FileHash $pkg -Algorithm SHA256).Hash
    if ($actual -ne $Sha256.ToUpperInvariant()) {
        Remove-Item $pkg -Force
        throw "conpty nupkg SHA-256 mismatch: expected $Sha256, got $actual"
    }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($pkg)
    try {
        $map = @{ "runtimes/win-$Arch/native/conpty.dll" = 'conpty.dll'; "build/native/runtimes/$Arch/OpenConsole.exe" = 'OpenConsole.exe' }
        foreach ($entry in $zip.Entries) {
            if ($map.ContainsKey($entry.FullName)) {
                [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, (Join-Path $dest $map[$entry.FullName]), $true)
            }
        }
    } finally { $zip.Dispose() }
}

foreach ($f in 'conpty.dll', 'OpenConsole.exe') {
    $p = Join-Path $dest $f
    if (-not (Test-Path $p)) { throw "$f missing in $dest" }
    $sig = Get-AuthenticodeSignature $p
    if ($sig.Status -ne 'Valid' -or $sig.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation') {
        Remove-Item $p -Force
        throw "$f is not validly Microsoft-signed ($($sig.Status)); removed"
    }
    # Copy-Item and zip extraction keep the file's original (old) timestamp, which cargo reads as
    # "unchanged since the last build" and so never reruns build.rs to copy it beside the exe.
    (Get-Item $p).LastWriteTime = Get-Date
    Write-Host "ok  $p  ($($sig.SignerCertificate.Subject.Split(',')[0]))"
}
Write-Host "Next: cargo build -p codeshell  (its build.rs, like term-spike's, copies these beside the exe)"
