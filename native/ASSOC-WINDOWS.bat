@echo off
rem OPTIONAL REPAIR TOOL. The game registers .deemopack automatically on
rem startup; run this only if the association was removed or blocked.
rem Register .deemopack for the current Windows user (no admin).
rem Keep this file next to deemo.exe in the release archive.
setlocal
set "EXE=%~dp0deemo.exe"
if not exist "%EXE%" (
  echo deemo.exe was not found next to this script.
  exit /b 1
)
reg add "HKCU\Software\Classes\DeemoPack" /ve /d "Deemo Pack" /f >nul
reg add "HKCU\Software\Classes\DeemoPack\DefaultIcon" /ve /d "\"%EXE%\",0" /f >nul
reg add "HKCU\Software\Classes\DeemoPack\shell\open\command" /ve /d "\"%EXE%\" \"%%1\"" /f >nul
reg add "HKCU\Software\Classes\.deemopack" /ve /d "DeemoPack" /f >nul
echo .deemopack is now associated with "%EXE%".
endlocal
