#!/usr/bin/env bash
# macOS signing, signature checks and the ONNX Runtime smoke for the
# supervised AFT module (ck-aft). Sourced by scripts/stage-card.sh and
# scripts/verify-placed-card.sh so staging and placement check the same rule.
#
# The rule: identifier ck-aft (capability grants key on it), hardened runtime,
# no get-task-allow, and exactly one hardened-runtime exception,
# com.apple.security.cs.disable-library-validation, which AFT needs to load
# the downloaded ONNX Runtime library (it has no Team ID).

CK_AFT_IDENTIFIER="ck-aft"
CK_AFT_ENTITLEMENTS="${CK_AFT_ENTITLEMENTS:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/ck-aft.entitlements.plist}"
CK_AFT_EXCEPTION="com.apple.security.cs.disable-library-validation"

# Sign in place with the pinned identifier, hardened runtime and the committed
# entitlements.
ck_aft_sign() {
  local binary="$1"
  codesign --force --sign - --identifier "$CK_AFT_IDENTIFIER" -o runtime \
    --entitlements "$CK_AFT_ENTITLEMENTS" "$binary"
}

# Read the signature back from the file itself and refuse anything but the
# rule above. Prints the codesign details and entitlements it judged.
# Usage: ck_aft_check_signature <binary> <error-prefix>
ck_aft_check_signature() {
  local binary="$1" prefix="$2"
  local details entitlements_xml flags plist keys key

  if ! codesign --verify --strict "$binary" 2>/dev/null; then
    echo "$prefix: signature invalid: codesign --verify --strict failed for $binary" >&2
    return 1
  fi
  details="$(codesign -dv "$binary" 2>&1)" || {
    echo "$prefix: signature unreadable: codesign -dv failed for $binary" >&2
    return 1
  }
  printf '%s\n' "$details" | sed -n -e '/^Identifier=/p' -e '/^CodeDirectory/p' -e '/^Runtime Version=/p' | sed 's/^/    /'

  if ! printf '%s\n' "$details" | grep -qx "Identifier=$CK_AFT_IDENTIFIER"; then
    echo "$prefix: signature identifier is not $CK_AFT_IDENTIFIER" >&2
    return 1
  fi
  flags="$(printf '%s\n' "$details" | sed -n 's/^CodeDirectory .*flags=0x[0-9a-f]*(\([^)]*\)).*/\1/p')"
  case ",$flags," in
    *,runtime,*) ;;
    *)
      echo "$prefix: hardened runtime missing: CodeDirectory flags are ($flags), expected runtime" >&2
      return 1
      ;;
  esac

  entitlements_xml="$(codesign -d --entitlements - --xml "$binary" 2>/dev/null)"
  echo "    entitlements: ${entitlements_xml:-<none>}"
  plist="$(mktemp "${TMPDIR:-/tmp}/ck-aft-entitlements.XXXXXX")"
  printf '%s' "$entitlements_xml" > "$plist"
  # PlistBuddy, not plutil: entitlement keys contain dots, which plutil reads
  # as a key path.
  keys="$(/usr/libexec/PlistBuddy -c Print "$plist" 2>/dev/null | sed -n 's/^    \([^ ]*\) = .*/\1/p')"
  if /usr/libexec/PlistBuddy -c "Print :com.apple.security.get-task-allow" "$plist" >/dev/null 2>&1; then
    rm -f "$plist"
    echo "$prefix: get-task-allow present: any same-user process could attach a debugger" >&2
    return 1
  fi
  if [ "$(/usr/libexec/PlistBuddy -c "Print :$CK_AFT_EXCEPTION" "$plist" 2>/dev/null)" != "true" ]; then
    rm -f "$plist"
    echo "$prefix: library validation not disabled: $CK_AFT_EXCEPTION is not true, so the ONNX Runtime load would fail" >&2
    return 1
  fi
  rm -f "$plist"
  for key in $keys; do
    if [ "$key" != "$CK_AFT_EXCEPTION" ]; then
      echo "$prefix: unexpected entitlement $key: the module carries only $CK_AFT_EXCEPTION" >&2
      return 1
    fi
  done
  echo "    signature ok: identifier $CK_AFT_IDENTIFIER, flags ($flags), exceptions: $CK_AFT_EXCEPTION"
}

# Run the one code path the exception exists for: a real ONNX Runtime load.
# A flags-only check would pass a binary whose semantic search is dead.
# Runs `warmup --only semantic` on a one-file project with an isolated HOME and
# storage, pointing ORT_DYLIB_PATH at an installed runtime.
# Usage: ck_aft_smoke_onnx <binary> <error-prefix>
ck_aft_smoke_onnx() {
  local binary="$1" prefix="$2"
  local ort="${CK_SMOKE_ORT_DYLIB:-$HOME/.local/share/cortexkit/aft/onnxruntime/1.24.4/libonnxruntime.dylib}"
  local models="${CK_SMOKE_MODEL_CACHE:-$HOME/.local/share/cortexkit/aft/semantic/models}"
  local work output status

  if [ ! -f "$ort" ]; then
    echo "$prefix: ONNX Runtime smoke cannot run: no runtime at $ort (set CK_SMOKE_ORT_DYLIB)" >&2
    return 1
  fi
  if [ ! -d "$models/models--Qdrant--all-MiniLM-L6-v2-onnx" ]; then
    echo "$prefix: ONNX Runtime smoke cannot run: no MiniLM model cache in $models (set CK_SMOKE_MODEL_CACHE)" >&2
    return 1
  fi
  work="$(mktemp -d "${TMPDIR:-/tmp}/ck-aft-smoke.XXXXXX")"
  mkdir -p "$work/home" "$work/storage/semantic" "$work/root"
  printf 'export function greetUser(name: string) {\n  return `hello ${name}`;\n}\n' > "$work/root/app.ts"
  # A copy, so the smoke never writes into the operator's model cache; `-c`
  # clones on APFS and costs no space.
  cp -Rc "$models" "$work/storage/semantic/models" 2>/dev/null || cp -R "$models" "$work/storage/semantic/models"

  status=0
  output="$(env -i PATH=/usr/bin:/bin HOME="$work/home" AFT_STORAGE_DIR="$work/storage" \
    ORT_DYLIB_PATH="$ort" "$binary" warmup --root "$work/root" --only semantic --timeout 120000 2>&1)" || status=$?
  rm -rf "$work"
  if [ "$status" -ne 0 ] || ! printf '%s\n' "$output" | grep -q '^aft warmup: semantic_index ready$'; then
    printf '%s\n' "$output" | grep -E 'semantic_index|warmup failed|ONNX|onnx' | sed 's/^/    /' >&2
    echo "$prefix: ONNX Runtime smoke failed: the signed binary could not load $ort (exit $status)" >&2
    return 1
  fi
  echo "    smoke: exercised $CK_AFT_EXCEPTION: loaded ONNX Runtime $ort (sha256 $(shasum -a 256 "$ort" | awk '{print $1}')) and built a semantic index"
}
