@echo off
chcp 65001 >nul
cd /d "%~dp0"
echo Deemo: проверка планшета, 15 секунд.
echo Води и нажимай пером по планшету.
start /wait "" deemo.exe --tablet-probe 15
echo Готово. Открываю tablet_probe.log
start "" tablet_probe.log
