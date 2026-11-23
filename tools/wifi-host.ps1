# Wi-Fi through the laptop for BedrockOS in VirtualBox (see src/wifihost.rs).
# The VM's COM2 is a named pipe (scripts/vbox.ps1 sets it up); this helper connects to it and answers:
#   HELLO                     -> AEONWIFI 1
#   SCAN                      -> NET<TAB>signal<TAB>secure<TAB>current<TAB>ssid lines, then END
#   CONNECT<TAB>ssid<TAB>pass -> OK text | ERR text   (the LAPTOP's Wi-Fi joins that network)
# Run it while the VM is running:  powershell -ExecutionPolicy Bypass -File tools\wifi-host.ps1
param([string]$Pipe = "bedrockos-wifi")

function Get-Current {
    $m = (netsh wlan show interfaces) | Select-String '^\s+SSID\s+:\s(.*)$' | Select-Object -First 1
    if ($m) { $m.Matches[0].Groups[1].Value.Trim() } else { "" }
}

function Get-Networks {
    $cur = Get-Current
    $nets = @(); $ssid = $null; $auth = ""; $sig = 0
    foreach ($l in (netsh wlan show networks mode=bssid)) {
        if ($l -match '^SSID \d+ : (.*)$') {
            if ($ssid) { $nets += ,@($ssid, $auth, $sig) }
            $ssid = $Matches[1].Trim(); $auth = ""; $sig = 0
        } elseif ($l -match '^\s+Authentication\s+:\s(.*)$') { $auth = $Matches[1].Trim() }
        elseif ($l -match '^\s+Signal\s+:\s(\d+)%') { $sig = [math]::Max($sig, [int]$Matches[1]) }
    }
    if ($ssid) { $nets += ,@($ssid, $auth, $sig) }
    foreach ($n in $nets) {
        if (-not $n[0]) { continue }
        $secure = if ($n[1] -eq "Open") { 0 } else { 1 }
        $current = if ($n[0] -eq $cur) { 1 } else { 0 }
        "NET`t$($n[2])`t$secure`t$current`t$($n[0])"
    }
    "END"
}

function Connect-Net([string]$ssid, [string]$pass) {
    $known = (netsh wlan show profiles) -match [regex]::Escape(": $ssid")
    if (-not $known) {
        if (-not $pass) { return "ERR this network needs a password" }
        $hex = -join ([Text.Encoding]::UTF8.GetBytes($ssid) | ForEach-Object { $_.ToString("X2") })
        $esc = [Security.SecurityElement]::Escape($ssid); $pw = [Security.SecurityElement]::Escape($pass)
        $xml = @"
<?xml version="1.0"?>
<WLANProfile xmlns="http://www.microsoft.com/networking/WLAN/profile/v1"><name>$esc</name>
<SSIDConfig><SSID><hex>$hex</hex><name>$esc</name></SSID></SSIDConfig><connectionType>ESS</connectionType>
<connectionMode>manual</connectionMode><MSM><security><authEncryption><authentication>WPA2PSK</authentication>
<encryption>AES</encryption><useOneX>false</useOneX></authEncryption><sharedKey><keyType>passPhrase</keyType>
<protected>false</protected><keyMaterial>$pw</keyMaterial></sharedKey></security></MSM></WLANProfile>
"@
        $f = Join-Path $env:TEMP "bedrockos-wifi.xml"
        Set-Content -Path $f -Value $xml -Encoding UTF8
        netsh wlan add profile filename="$f" | Out-Null
        Remove-Item $f
    }
    $r = netsh wlan connect name="$ssid"
    for ($i = 0; $i -lt 20; $i++) {
        Start-Sleep -Milliseconds 500
        if ((Get-Current) -eq $ssid) { return "OK the laptop joined $ssid" }
    }
    "ERR the laptop could not join $ssid ($r)"
}

while ($true) {
    $p = New-Object IO.Pipes.NamedPipeClientStream(".", $Pipe, [IO.Pipes.PipeDirection]::InOut)
    try { $p.Connect(3000) } catch { Write-Host "waiting for the BedrockOS VM (pipe $Pipe)..."; continue }
    Write-Host "connected to BedrockOS"
    $rd = New-Object IO.StreamReader($p); $wr = New-Object IO.StreamWriter($p); $wr.NewLine = "`n"; $wr.AutoFlush = $true
    try {
        while ($null -ne ($line = $rd.ReadLine())) {
            $f = $line.TrimEnd("`r").Split("`t")
            switch ($f[0]) {
                "HELLO" { $wr.WriteLine("AEONWIFI 1") }
                "SCAN" { Get-Networks | ForEach-Object { $wr.WriteLine($_) } }
                "CONNECT" { $wr.WriteLine((Connect-Net $f[1] ($(if ($f.Count -gt 2) { $f[2] } else { "" })))) }
                default { if ($f[0]) { $wr.WriteLine("ERR unknown command") } }
            }
        }
    } catch { Write-Host "pipe closed: $_" }
    $p.Dispose()
}
