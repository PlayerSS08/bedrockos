@echo off
rem Double-click to run BedrockOS. Works from any folder, on any Windows PC with VirtualBox 7.x installed:
rem the first run creates the "BedrockOS" VM (scripts\vbox.ps1), later runs just start it.
rem Python 3 (optional) runs the laptop helper for Wi-Fi and https pages in the browser.
title BedrockOS launcher
set "REPO=%~dp0"
if "%REPO:~-1%"=="\" set "REPO=%REPO:~0,-1%"

set "VBM=C:\Program Files\Oracle\VirtualBox\VBoxManage.exe"
if defined VBOX_MSI_INSTALL_PATH if exist "%VBOX_MSI_INSTALL_PATH%VBoxManage.exe" set "VBM=%VBOX_MSI_INSTALL_PATH%VBoxManage.exe"
if not exist "%VBM%" (
    echo VirtualBox was not found. Install VirtualBox 7.x from https://www.virtualbox.org/ and run this again.
    pause
    exit /b 1
)

rem 1. The VM (skipped when it already runs). vbox.ps1 creates it if needed, attaches out\bedrockos.vdi and starts it.
"%VBM%" list runningvms | findstr /c:"\"BedrockOS\"" >nul
if errorlevel 1 (
    echo Starting the BedrockOS virtual machine...
    powershell -NoProfile -ExecutionPolicy Bypass -File "%REPO%\scripts\vbox.ps1"
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

rem 2. The helper (optional), started after the VM has opened its COM2 pipe, in its own minimised window
rem    (skipped when it already runs: port 8086 is taken, or when Python is not installed).
where python >nul 2>&1
if errorlevel 1 (
    echo Python not found: Wi-Fi and https pages in the browser will not work. Everything else does.
    goto done
)
netstat -ano | findstr /r /c:":8086 .*LISTENING" >nul
if errorlevel 1 (
    echo Starting the BedrockOS helper...
    timeout /t 5 /nobreak >nul
    start "BedrockOS helper - keep this open" /min /d "%REPO%" python tools\bedrock-host.py
) else (
    echo The helper is already running.
)

:done
echo.
echo Sign in as BedrockOS, then at the BedrockOS menu press Enter on "Boot BedrockOS".
timeout /t 8 >nul