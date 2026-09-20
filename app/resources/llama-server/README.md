# llama-server

The pinned llama.cpp build that serves Qwen3-ASR. Only this file is committed; fetch the
binaries with `node scripts/fetch-llama-server.mjs`. The app looks here first, then on
`PATH`, then in the usual Homebrew locations. `ZILLANOTE_LLAMA_SERVER` overrides all of it.
