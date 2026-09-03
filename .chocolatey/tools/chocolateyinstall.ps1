$ErrorActionPreference = 'Stop'

$packageName = 'codeshellmanager'
$toolsDir    = "$(Split-Path -Parent $MyInvocation.MyCommand.Definition)"

# These placeholders are replaced by .github/workflows/chocolatey.yml at pack time
# with the version-pinned MSI URL and its SHA256 from the GitHub Release.
$url64       = '__URL64__'
$checksum64  = '__CHECKSUM64__'

$logPath = "$env:TEMP\$packageName.$env:chocolateyPackageVersion.MsiInstall.log"

# CodeShellManager renders its terminals in WebView2, so the Evergreen Runtime is a hard
# requirement at run time. It ships with Windows 11 and with recent Windows 10, so this
# WARNS rather than fails: a missing runtime is worth telling the user about, but a
# detection miss must not block an install that would have worked.
#
# Deliberately a check and not a <dependency> on webview2-runtime — that would pull and
# install the runtime on every machine, including the majority that already have it.
$wv2Clients = @(
  'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
  'HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
  'HKCU:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
)
$wv2Version = $null
foreach ($key in $wv2Clients) {
  try {
    $pv = (Get-ItemProperty -Path $key -Name pv -ErrorAction Stop).pv
    if ($pv -and $pv -ne '0.0.0.0') { $wv2Version = $pv; break }
  } catch { }
}

if ($wv2Version) {
  Write-Host "Microsoft Edge WebView2 Runtime detected (version $wv2Version)."
} else {
  Write-Warning @'
Microsoft Edge WebView2 Runtime was not detected.

CodeShellManager renders its terminals in WebView2 and will not display them without it.
It is included with Windows 11 and recent Windows 10 builds; if terminals appear blank
after install, get the Evergreen Runtime from:

    https://developer.microsoft.com/en-us/microsoft-edge/webview2/

or run:  choco install webview2-runtime
'@
}

$packageArgs = @{
  packageName    = $packageName
  unzipLocation  = $toolsDir
  fileType       = 'msi'
  url64bit       = $url64
  softwareName   = 'CodeShellManager*'
  checksum64     = $checksum64
  checksumType64 = 'sha256'
  silentArgs     = "/qn /norestart /l*v `"$logPath`""
  validExitCodes = @(0, 3010, 1641)
}

Install-ChocolateyPackage @packageArgs
