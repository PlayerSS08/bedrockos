@echo off
rem Double-click to run BedrockOS: starts the laptop helper (Wi-Fi + https pages) and the VirtualBox VM.
rem Works from anywhere (for example a copy on the Desktop).
title BedrockOS launcher
set "REPO=D:\BedrockOS\bedrockos"
set "VBM=C:\Program Files\Oracle\VirtualBox\VBoxManage.exe"

rem 1. The VM (skipped when it already runs).
"%VBM%" list runningvms | findstr /c:"\"BedrockOS\"" >nul
if errorlevel 1 (
    echo Starting the BedrockOS virtual machine...
    "%VBM%" startvm BedrockOS --type gui
    if errorlevel 1 (
        echo.
        echo Could not start the VM. If it hangs at the VirtualBox logo, power it off and run:
        echo   "%VBM%" modifynvram BedrockOS inituefivarstore
        echo   "%VBM%" setextradata BedrockOS VBoxInternal2/EfiGraphicsResolution 1280x800
        pause
        exit /b 1
    )
) else (
    echo BedrockOS is already running - look for its VirtualBox window.
)

rem 2. The helper, started after the VM has opened its COM2 pipe, in its own minimised window (skipped when it already runs: port 8086 is taken).
netstat -ano | findstr /r /c:":8086 .*LISTENING" >nul
if errorlevel 1 (
    echo Starting the BedrockOS helper...
    timeout /t 5 /nobreak >nul
    start "BedrockOS helper - keep this open" /min /d "%REPO%" python tools\bedrock-host.py
) else (
    echo The helper is already running.
)

echo.
echo At the BedrockOS menu press Enter on "Boot BedrockOS".
timeout /t 6 >nul
