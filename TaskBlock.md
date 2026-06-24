# Task Block — ADR-0013 / ADR-0014

All groups complete. Group F validation confirmed:
- Gate 2 (RTSP interruption → recovery): ✅
- Gate 3 (SIGKILL → graceful respawn): ✅
- Gate 4 (credentials not in ps/logs): ✅
- Gate 1 (offline-at-startup → StreamsChanged): code path implemented; live camera control needed for end-to-end test.
