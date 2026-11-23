<#
.SYNOPSIS
  One-command VirtualBox runner for BedrockOS (Windows host).

.EXAMPLE
  .\scripts\vbox.ps1                 # build if needed, create/refresh the VM, attach the disk, start it
  .\scripts\vbox.ps1 -Headless       # same, without a window (serial log in out\serial.log)
  .\scripts\vbox.ps1 -Res 1920x1080  # firmware screen resolution (default 1280x800: fits a laptop screen at 125% scaling)
  .\scripts\vbox.ps1 -Action stop | delete | screenshot
#>
param(
    [ValidateSet('start', 'stop', 'delete', 'screenshot', 'create')]
    [string]$Action = 'start',
    [string]$Res = '1280x800',
    [switch]$Headless,
    [switch]$Build,
    [ValidateSet('nat', 'bridged', 'intnet', 'none')]
    [string]$Net = 'nat',
    [string]$BridgeAdapter = '',
    [string]$Shot = ''
)
$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
$Out = Join-Path $Root 'out'
$VM = 'BedrockOS'
$Vdi = Join-Path $Out 'bedrockos.vdi'
$Serial = Join-Path $Out 'serial.log'

$VBM = (Get-Command VBoxManage -ErrorAction SilentlyContinue).Source
if (-not $VBM) {
    foreach ($p in @("$env:VBOX_MSI_INSTALL_PATH\VBoxManage.exe", 'C:\Program Files\Oracle\VirtualBox\VBoxManage.exe')) {
        if ($p -and (Test-Path $p)) { $VBM = $p; break }
    }
}
if (-not $VBM) { throw 'VBoxManage not found. Install VirtualBox 7.x (winget install Oracle.VirtualBox).' }

function VB { & $VBM @args; if ($LASTEXITCODE -ne 0) { throw "VBoxManage $($args -join ' ') failed ($LASTEXITCODE)" } }
function VBq {
    # Best-effort call: failures (e.g. nothing attached yet) are expected and ignored.
    $old = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
    try { & $VBM @args 2>&1 | Out-Null } catch { } finally { $ErrorActionPreference = $old }
}
function Test-VM { (& $VBM list vms) -match "^`"$VM`" " }
function Test-Running { (& $VBM list runningvms) -match "^`"$VM`" " }

function New-BedrockVM {
    if (Test-VM) { return }
    VB createvm --name $VM --ostype Other_64 --register --basefolder (Join-Path $Out 'vm')
    VB modifyvm $VM --firmware efi64 --memory 1024 --cpus 2 --ioapic on --rtcuseutc on `
        --graphicscontroller vmsvga --vram 128 --audio-driver none `
        --usbxhci on --mouse usbtablet --keyboard ps2 `
        --uart1 0x3F8 4 --uart-mode1 file $Serial `
        --boot1 disk --boot2 none --boot3 none --boot4 none
    VB storagectl $VM --name SATA --add sata --controller IntelAhci --portcount 2
}

function Set-Network {
    switch ($Net) {
        'nat'     { VB modifyvm $VM --nic1 nat --nictype1 82540EM --cableconnected1 on }
        'bridged' {
            if (-not $BridgeAdapter) { throw 'Bridged mode needs -BridgeAdapter "<name from VBoxManage list bridgedifs>"' }
            VB modifyvm $VM --nic1 bridged --bridgeadapter1 $BridgeAdapter --nictype1 82540EM --cableconnected1 on
        }
        'intnet'  { VB modifyvm $VM --nic1 intnet --intnet1 bedrock-wifi-sim --nictype1 82540EM --cableconnected1 on }
        'none'    { VB modifyvm $VM --nic1 none }
    }
}

function Set-Disk {
    VBq storageattach $VM --storagectl SATA --port 0 --device 0 --medium none
    VBq closemedium disk $Vdi
    VB storageattach $VM --storagectl SATA --port 0 --device 0 --type hdd --medium $Vdi
}

switch ($Action) {
    'stop'   { if (Test-Running) { VB controlvm $VM poweroff }; return }
    'delete' { if (Test-Running) { VB controlvm $VM poweroff; Start-Sleep 2 }; if (Test-VM) { VB unregistervm $VM --delete-all }; return }
    'screenshot' {
        if (-not $Shot) { $Shot = Join-Path $Out ("shot-{0:yyyyMMdd-HHmmss}.png" -f (Get-Date)) }
        VB controlvm $VM screenshotpng $Shot; Write-Output $Shot; return
    }
}

if ($Build -or -not (Test-Path $Vdi)) {
    Push-Location $Root; try { cargo xtask build; if ($LASTEXITCODE -ne 0) { throw 'build failed' } } finally { Pop-Location }
}
if (Test-Running) { VB controlvm $VM poweroff; Start-Sleep 2 }
New-BedrockVM
# USB tablet: absolute positions, so the host pointer moves in and out of the VM window freely (mouse
# integration); BedrockOS reads its reports. Also updates VMs made by older scripts.
VB modifyvm $VM --mouse usbtablet
Set-Network
VB setextradata $VM VBoxInternal2/EfiGraphicsResolution $Res
Set-Disk
if ($Action -eq 'create') { Write-Output "VM '$VM' ready."; return }
if (Test-Path $Serial) { Remove-Item $Serial -Force }
VB startvm $VM --type ($(if ($Headless) { 'headless' } else { 'gui' }))
Write-Output "Started '$VM' ($Res, network $Net). Serial log: $Serial"
