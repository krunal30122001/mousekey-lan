; Installer hooks: open the Windows Firewall for MouseKey LAN (TCP 47820 + mDNS discovery)
; so the first connection does not silently fail behind a firewall prompt, and keep the
; optional lock-screen service (mkl-service.exe) in step with the installed files.

!macro NSIS_HOOK_PREINSTALL
  ; The lock-screen service and its agent hold mkl-service.exe open; "net stop" waits for both.
  nsExec::ExecToLog 'net stop MouseKeyLANService'
  nsExec::ExecToLog 'taskkill /F /IM mkl-service.exe'
!macroend

!macro NSIS_HOOK_POSTINSTALL
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="MouseKey LAN"'
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="MouseKey LAN" dir=in action=allow program="$INSTDIR\mousekey-lan.exe" enable=yes profile=any'
  ; Repairs and restarts the lock-screen service if it was installed; otherwise does nothing.
  nsExec::ExecToLog '"$INSTDIR\mkl-service.exe" --upgrade'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::ExecToLog '"$INSTDIR\mkl-service.exe" --uninstall'
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="MouseKey LAN"'
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "MouseKey LAN"
  nsExec::ExecToLog 'schtasks /Delete /F /TN "MouseKey LAN (administrator)"'
  RMDir /r "$INSTDIR\logs"
  RMDir "$INSTDIR"
!macroend
