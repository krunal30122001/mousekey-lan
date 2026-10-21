; Installer hooks: open the Windows Firewall for MouseKey LAN (TCP 47820 + mDNS discovery)
; so the first connection does not silently fail behind a firewall prompt.

!macro NSIS_HOOK_POSTINSTALL
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="MouseKey LAN"'
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="MouseKey LAN" dir=in action=allow program="$INSTDIR\mousekey-lan.exe" enable=yes profile=any'
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="MouseKey LAN"'
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "MouseKey LAN"
  nsExec::ExecToLog 'schtasks /Delete /F /TN "MouseKey LAN (administrator)"'
!macroend
