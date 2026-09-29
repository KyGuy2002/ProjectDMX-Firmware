@echo off
setlocal EnableDelayedExpansion

rem Converts every .mp3 under this script's folder (recursively) to the mono
rem IMA ADPCM .wav format the board plays cheaply, and collects them all flat
rem in an "output" folder next to this script. Copy that folder's contents to
rem the SD card and change the extensions in config.jsonc from .mp3 to .wav.
rem
rem Files are flattened by name, so two mp3s with the same name in different
rem folders end up as one .wav (the last one converted wins).
rem
rem Needs ffmpeg on PATH (e.g. "winget install ffmpeg").

set "ROOT=%~dp0"
set "OUT=%ROOT%output"

where ffmpeg >nul 2>&1
if errorlevel 1 (
    echo ffmpeg not found on PATH. Install it with: winget install ffmpeg
    pause
    exit /b 1
)

if not exist "%OUT%" mkdir "%OUT%"

set /a OK=0
set /a FAILED=0

for /r "%ROOT%" %%F in (*.mp3) do (
    rem Skip anything already inside the output folder.
    if /i not "%%~dpF"=="%OUT%\" (
        echo Converting %%F
        ffmpeg -hide_banner -loglevel error -y -i "%%F" -map_metadata -1 -fflags +bitexact -ac 1 -ar 44100 -c:a adpcm_ima_wav "%OUT%\%%~nF.wav"
        if errorlevel 1 (
            echo   FAILED: %%F
            set /a FAILED+=1
        ) else (
            set /a OK+=1
        )
    )
)

echo.
echo Done: !OK! converted, !FAILED! failed. Output: %OUT%
pause
