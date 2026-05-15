$ErrorActionPreference = 'Stop'

$packageName = 'codeshellmanager'
$toolsDir    = "$(Split-Path -Parent $MyInvocation.MyCommand.Definition)"

# These placeholders are replaced by .github/workflows/chocolatey.yml at pack time
# with the version-pinned MSI URL and its SHA256 from the GitHub Release.
$url64       = '__URL64__'
$checksum64  = '__CHECKSUM64__'

$logPath = "$env:TEMP\$packageName.$env:chocolateyPackageVersion.MsiInstall.log"

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
