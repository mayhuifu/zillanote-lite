# Security

ZillaNote lite runs on your computer. Audio does not leave it, unless you choose a speech
service in Settings, which then gets each stretch of speech to transcribe; the transcript goes
only to the language-model endpoint you configure, and the minutes to the mail server you
configure.
The speech engine it starts listens on `127.0.0.1` on a port of its own choosing; downloads
are checked against pinned SHA-256 checksums; the API keys and mail password go to the
macOS keychain (on Windows they stay in the settings file of your user profile for now).

If you find a way round any of that, or any other vulnerability, please do not open a
public issue. Use GitHub's private vulnerability reporting on this repository ("Security"
tab, "Report a vulnerability"), or write to the maintainer at the address on the commits.
You will get an answer within a week, and credit in the release that fixes it if you want
it.

Only the latest release is supported.
