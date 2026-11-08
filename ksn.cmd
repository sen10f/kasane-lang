@echo off
set EXE=%~dp0stage0\target\release\ksn.exe
if not exist "%EXE%" (
  echo build first: cd stage0 ^&^& cargo build --release 1>&2
  exit /b 1
)
"%EXE%" %*
