; Run by the installer around installing and uninstalling Celsus.

; The MCP server is started by other apps (Claude, Cursor and the like) and keeps running while they are open.
; A running program cannot be replaced or removed, so the copy in the install folder is stopped first; the apps
; start it again when they next need it.
!macro StopMcpServer
  nsExec::Exec 'powershell -NoProfile -NonInteractive -Command "Get-Process celsus-mcp -ErrorAction SilentlyContinue | Where-Object { $$_.Path -like \"$INSTDIR\*\" } | Stop-Process -Force"'
  Pop $0
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro StopMcpServer
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro StopMcpServer
!macroend
