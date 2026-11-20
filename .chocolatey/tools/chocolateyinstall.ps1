$ErrorActionPreference = 'Stop'

$packageName = 'codeshellmanager-beta'
$toolsDir    = "$(Split-Path -Parent $MyInvocation.MyCommand.Definition)"

# These placeholders are replaced by .github/workflows/chocolatey-beta.yml at pack time
# with the version-pinned MSI URL and its SHA256 from the GitHub pre-release.
$url64       = '__URL64__'
$checksum64  = '__CHECKSUM64__'

$logPath = "$env:TEMP\$packageName.$env:chocolateyPackageVersion.MsiInstall.log"

# CodeShellManager 2 draws its terminals itself: no WebView2 runtime is needed.
$packageArgs = @{
  packageName    = $packageName
  unzipLocation  = $toolsDir
  fileType       = 'msi'
  url64bit       = $url64
  softwareName   = 'CodeShellManager 2 Beta*'
  checksum64     = $checksum64
  checksumType64 = 'sha256'
  silentArgs     = "/qn /norestart /l*v `"$logPath`""
  validExitCodes = @(0, 3010, 1641)
}

Install-ChocolateyPackage @packageArgs
