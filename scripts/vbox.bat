@echo off
rem One-click BedrockOS in VirtualBox (Windows). Arguments are passed to vbox.ps1, e.g.  vbox.bat -Res 1920x1080
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0vbox.ps1" %*
