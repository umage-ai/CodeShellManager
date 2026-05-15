$ErrorActionPreference = 'Stop'

$packageName = 'codeshellmanager'
$softwareName = 'CodeShellManager*'

# Discover the install record (MSI product code) registered for the
# installed CodeShellManager and uninstall via msiexec.
$key = Get-UninstallRegistryKey -SoftwareName $softwareName

if ($key.Count -eq 1) {
  $packageArgs = @{
    packageName    = $packageName
    fileType       = 'msi'
    silentArgs     = "$($key.PSChildName) /qn /norestart"
    validExitCodes = @(0, 3010, 1605, 1614, 1641)
    file           = ''
  }
  Uninstall-ChocolateyPackage @packageArgs
}
elseif ($key.Count -eq 0) {
  Write-Warning "$packageName has already been uninstalled by other means."
}
elseif ($key.Count -gt 1) {
  Write-Warning "$($key.Count) matches found!"
  Write-Warning "To prevent accidental data loss, no programs will be uninstalled."
  Write-Warning "Please alert package maintainer the following keys were matched:"
  $key | ForEach-Object { Write-Warning "- $($_.DisplayName)" }
}
