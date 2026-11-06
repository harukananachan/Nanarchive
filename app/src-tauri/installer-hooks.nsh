; Nanarchive installer hooks.

; Before uninstalling: remove the Explorer menu and the .nax file type
; the app added for this user.
!macro NSIS_HOOK_PREUNINSTALL
  ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" --unregister-shell'
!macroend