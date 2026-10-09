from the repo root, create the CA below; `ca.crt` is exported to the guest and `ca.key` stays on the host (ignored by git); set `GATEWAY_UID/GID` to match compose if overriding the defaults, including through `.env`.

```sh
umask 077
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 \
  -out connect-proxy/ca.key
openssl req -new -x509 -sha256 -days 3650 \
  -key connect-proxy/ca.key -out connect-proxy/ca.crt \
  -subj '/CN=ggez CONNECT CA' \
  -addext 'basicConstraints=critical,CA:TRUE,pathlen:0' \
  -addext 'keyUsage=critical,keyCertSign,cRLSign' \
  -addext 'subjectKeyIdentifier=hash'
chmod 644 connect-proxy/ca.crt
sudo chown "${GATEWAY_UID:-65532}:${GATEWAY_GID:-65532}" connect-proxy/ca.key
```
