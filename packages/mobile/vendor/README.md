# Vendored MeshCore Java libraries

Source copied from `meshmore-sns` at commit `7386999` (`CAPACITY: the radio says how
much it holds, and says when it is full`), 2026-11-02.

- `libmeshcore` — pure-Java companion-protocol codec/crypto/models.
- `libmeshcore-android` — Android BLE transport and `MeshcoreSession`.

Local changes in this tree are marked with `LDMUX` comments:

- raw custom send (`CMD_SEND_RAW_DATA`, 0x19), receive (`PUSH_CODE_RAW_DATA`, 0x84), and
  a send-result contract on `MeshcoreSession`;
- the raw frame/size constants and tests used by the LDMUX adapter.

License: MIT, reproduced in each library's `LICENSE`.
