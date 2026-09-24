#!/bin/bash
set -euo pipefail

cd "$(dirname "$0")/../.."
required=(APPLE_DEVELOPER_ID_APPLICATION_P12_BASE64 APPLE_DEVELOPER_ID_APPLICATION_P12_PASSWORD APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID)
missing=()
configured=0
for name in "${required[@]}"; do
  if [[ -n "${!name:-}" ]]; then
    configured=$((configured + 1))
  else
    missing+=("$name")
  fi
done
if [[ "$configured" -gt 0 && ${#missing[@]} -gt 0 ]]; then
  echo "Missing GitHub Secrets: ${missing[*]}" >&2
  exit 1
fi

signing_identity="-"
signing_args=(--timestamp=none)
signing_dir=""
keychain=""
original_keychains=()
cleanup() {
  if [[ -n "$keychain" ]]; then
    security list-keychains -d user -s "${original_keychains[@]}" >/dev/null 2>&1 || true
    security delete-keychain "$keychain" >/dev/null 2>&1 || true
  fi
  if [[ -n "$signing_dir" ]]; then rm -rf "$signing_dir"; fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# electron-builder 26 会把 identity="-" 当作证书名称的子串匹配；开发机有带连字符的
# 自签证书时，可能误用它。无发布凭据也使用空的隔离钥匙串，确保真正生成 ad-hoc 包。
signing_dir="$(mktemp -d "${RUNNER_TEMP:?GitHub RUNNER_TEMP is required}/zcode-signing.XXXXXX")"
keychain="$signing_dir/build.keychain-db"
keychain_password="$(node -e 'process.stdout.write(require("node:crypto").randomBytes(32).toString("hex"))')"
while IFS= read -r entry; do
  original_keychains+=("$entry")
done < <(security list-keychains -d user | sed -E 's/^[[:space:]]*"//; s/"[[:space:]]*$//')
security create-keychain -p "$keychain_password" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"
security list-keychains -d user -s "$keychain" "${original_keychains[@]}"
export CSC_KEYCHAIN="$keychain"

if [[ "$configured" -gt 0 ]]; then
  # 只收紧证书文件权限；全局 umask 077 会污染安装包内资源权限，把普通 0644 文件变成 0600。
  (umask 077; printf '%s' "$APPLE_DEVELOPER_ID_APPLICATION_P12_BASE64" | base64 -D > "$signing_dir/certificate.p12")
  # 非交互 runner 必须导入私钥并设置 partition-list；仅导入证书会在 codesign 时失败。
  # security 的原始输出含证书条目信息，因此只输出失败步骤，不回放原始内容。
  if ! security import "$signing_dir/certificate.p12" -k "$keychain" \
    -P "$APPLE_DEVELOPER_ID_APPLICATION_P12_PASSWORD" -T /usr/bin/codesign -T /usr/bin/security >/dev/null 2>&1; then
    echo "Unable to import Apple signing certificate" >&2
    exit 1
  fi
  if ! security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$keychain_password" "$keychain" >/dev/null 2>&1; then
    echo "Unable to configure signing key access" >&2
    exit 1
  fi
  identities="$(security find-identity -v -p codesigning "$keychain")"
  identity_count="$(printf '%s\n' "$identities" | awk '/valid identities found/ { print $1 }')"
  if [[ "$identity_count" != 1 ]] || ! printf '%s\n' "$identities" | grep -q 'Developer ID Application:'; then
    echo "Apple P12 must contain exactly one valid Developer ID Application signing identity" >&2
    exit 1
  fi
  signing_identity="$(printf '%s\n' "$identities" | awk '/^[[:space:]]*[0-9]+\)/ { print $2; exit }')"
  echo "::add-mask::$signing_identity"
  export APPLE_SIGNING_IDENTITY="$signing_identity"
  export ZCODE_ENABLE_MAC_SIGN=1
  signing_args=(--keychain "$keychain" --timestamp --options runtime)
else
  export ZCODE_ENABLE_MAC_SIGN=0
  echo "Apple Secrets are not configured; building an ad-hoc signed test app without notarization."
fi

# electron-builder 的 signIgnore 跳过 glm/tools；这两棵树必须先签名，不能只签外层 app。
for runtime_dir in packages/desktop/bundled-agents/darwin-arm64/glm packages/desktop/bundled-tools/darwin-arm64; do
  test -d "$runtime_dir"
  while IFS= read -r -d '' binary; do
    if file -b "$binary" | grep -q 'Mach-O'; then
      codesign --force --sign "$signing_identity" "${signing_args[@]}" "$binary"
      codesign --verify --strict "$binary"
    fi
  done < <(find "$runtime_dir" -type f \( -perm -111 -o -name '*.node' -o -name '*.dylib' -o -name '*.jnilib' \) -print0)
done

pnpm bundle:desktop -- --os mac --arch arm64 --skip-prepare --skip-build
dist="packages/desktop/${ZCODE_DESKTOP_DIST_DIR:-dist}"
shopt -s nullglob
apps=("$dist"/mac-arm64/*.app)
dmgs=("$dist"/*mac-arm64.dmg)
if [[ ${#apps[@]} != 1 || ${#dmgs[@]} != 1 ]]; then
  echo "Expected exactly one macOS ARM64 app and DMG" >&2
  exit 1
fi
codesign --verify --deep --strict "${apps[0]}"

if [[ "$configured" -eq 0 ]]; then exit 0; fi
codesign --force --sign "$signing_identity" "${signing_args[@]}" "${dmgs[0]}"
xcrun notarytool submit "${dmgs[0]}" --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" \
  --team-id "$APPLE_TEAM_ID" --wait --timeout 20m --output-format json > "$signing_dir/notarization.json"
# notarytool 的命令成功不等于审核 Accepted；必须读回服务端状态后才允许 staple/upload。
node --input-type=module - "$signing_dir/notarization.json" <<'NODE'
import { readFile } from 'node:fs/promises';
const result = JSON.parse(await readFile(process.argv[2], 'utf8'));
if (result.status !== 'Accepted') throw new Error(`Apple notarization ${result.status} (request ${result.id})`);
console.log(`Apple notarization accepted (request ${result.id})`);
NODE
xcrun stapler staple "${dmgs[0]}"
xcrun stapler validate "${dmgs[0]}"
codesign --verify --strict "${dmgs[0]}"
