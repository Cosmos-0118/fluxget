@echo off
setlocal
powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1" %*
set "install_status=%errorlevel%"
if not "%install_status%"=="0" echo Fluxget installation failed. See the error above.
if "%~1"=="" pause
exit /b %install_status%
