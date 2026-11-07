# Privacy

ldmux collects nothing.

The app connects only to a machine you name yourself: your own computer, reached over a private
tailnet or over a radio link. There is no account, no sign-in, and no server run by the developer.

- No analytics, no crash reporting, no telemetry. The app contains no such SDK, so there is
  nothing to report and nothing to opt out of.
- No data leaves your device except to your own machine. What you type is sent to the host you
  paired with. It is not sent anywhere else.
- The pairing token is stored on the device and excluded from backup, so the system cannot move it
  off the phone.
- The camera is not used. Pairing scans a QR code through Google's Play services code scanner,
  which decodes on the device and sends nothing to the developer.
- Speech is optional and separate. Dictation uses Android's own speech recogniser, and if the
  device's recogniser is cloud-based, audio may go to that provider. That is the device's setting,
  not something this app controls, and the app says so when it happens.

Play's data safety form follows from the above: no data collected, no data shared, nothing stored by
the developer, and therefore nothing to delete on request.
