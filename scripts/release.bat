@echo off
setlocal

if "%~1"=="" (
    echo Usage: %~nx0 vMAJOR.MINOR.PATCH
    echo Example: %~nx0 v0.1.2
    exit /b 1
)

set "VERSION=%~1"
set "SCRIPT_DIR=%~dp0"

echo Releasing %VERSION% ...
powershell -NoProfile -ExecutionPolicy Bypass -File "%SCRIPT_DIR%release.ps1" -Version "%VERSION%" -PushTag
if errorlevel 1 (
    echo Release failed.
    exit /b 1
)

echo Release triggered successfully for %VERSION%.
exit /b 0
