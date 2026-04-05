@echo off
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0npm.ps1" %*
exit /b %ERRORLEVEL%
