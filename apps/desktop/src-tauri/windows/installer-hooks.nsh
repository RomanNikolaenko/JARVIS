; NSIS installer hooks (tauri.conf.json -> bundle.windows.nsis.installerHooks).

; The NSIS installer itself runs on every Windows, so the ARM64 installer
; would happily install a program an x64 PC cannot start ("This app can't
; run on your PC"). It refuses instead and names the right file. The MSI
; needs no such check: Windows Installer refuses a package for another
; processor. x64 installers stay allowed on ARM64 (Windows 11 emulates x64).
!macro NSIS_HOOK_PREINSTALL
  !if "${ARCH}" == "arm64"
    ${IfNot} ${IsNativeARM64}
      MessageBox MB_ICONSTOP|MB_OK "This installer is for Windows on ARM (ARM64 processors such as Snapdragon).$\r$\n$\r$\nThis PC needs JARVIS_${VERSION}_x64-setup.exe from the same release." /SD IDOK
      ; The section has just created the install folder; only an empty one goes.
      SetOutPath $TEMP
      RMDir $INSTDIR
      SetErrorLevel 3
      Quit
    ${EndIf}
  !endif
!macroend

; Tauri's template keeps the progress page open after installing
; (MUI_FINISHPAGE_NOAUTOCLOSE), so the user had to press Next on
; "Installation Complete". Go on to the finish page by itself.
!macro NSIS_HOOK_POSTINSTALL
  SetAutoClose true
!macroend
