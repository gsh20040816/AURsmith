# 下载证书链

`sectigo-dv-e36.pem` 是 Sectigo Public Server Authentication CA DV E36 中间证书，
来自证书 AIA 指定的 http://crt.sectigo.com/SectigoPublicServerAuthenticationCADVE36.crt
（DER 转 PEM）。MotionPro 下载站在 2026-10-10 未发送此中间证书。

构建镜像时先用 Arch 系统根证书执行 `openssl verify`，成功才加入 curl 专用 CA bundle。
不新增私有根证书、不关闭 TLS 验证；makepkg 仍校验源文件摘要。仅构建容器设置
`CURL_CA_BUNDLE`，不修改宿主、服务端或签名器的信任库。
