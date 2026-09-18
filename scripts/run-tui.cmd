@echo off
setlocal
rem 1H-Agent TUI launch helper: forwards all arguments to run-tui.ps1.
where pwsh >nul 2>nul
if %ERRORLEVEL%==0 (
  pwsh -NoProfile -ExecutionPolicy Bypass -File "%~dp0run-tui.ps1" %*
) else (
  powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0run-tui.ps1" %*
)
exit /b %ERRORLEVEL%
