; Tauri NSIS 安装钩子。
;
; VC++ 运行库 app-local 部署:SnipStack.exe 直接导入 msvcp140.dll / msvcp140_1.dll
; (来自 ONNX Runtime 的 C++ 静态库,/MD 编译,crt-static 管不到它),全新 Windows
; 不自带 VC++ Redistributable,缺失时报「找不到 MSVCP140_1.dll」。DLL 经 bundle
; resources 打进 resources\vcredist\,安装后拷到安装根目录——加载器在 exe 同目录
; 即可找到,用户无需另装运行库。更新器走同一安装包,钩子同样生效。

!macro NSIS_HOOK_POSTINSTALL
  CopyFiles /SILENT "$INSTDIR\resources\vcredist\*.dll" "$INSTDIR"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  Delete "$INSTDIR\msvcp140*.dll"
  Delete "$INSTDIR\vcruntime140*.dll"
  Delete "$INSTDIR\concrt140.dll"
  Delete "$INSTDIR\vccorlib140.dll"
!macroend
