@echo off
cd /d "%~dp0"
node benchmark.cjs %*
echo.
pause
