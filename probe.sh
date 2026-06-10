#!/bin/bash
cd /c/tmp/tfm2-ai-mod/q
export PATH="/c/Users/veria/.cargo/bin:$PATH"
DEPS="/c/tmp/tfm2-ai-mod/sdk/mod-sdk/deps"; RLIB=$(ls "$DEPS"/libmod_api-*.rlib|head -1)
export CARGO_ENCODED_RUSTFLAGS=$(printf -- '-L\x1fdependency=%s\x1f--extern\x1fmod_api=%s' "$(cygpath -w "$DEPS")" "$(cygpath -w "$RLIB")")
cargo rustc --release --lib -- --crate-type cdylib 2>&1 | tr -d '\r' | grep -iE "expected .* found|missing fields|no field|no variant" | head -3
