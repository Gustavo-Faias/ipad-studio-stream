#!/usr/bin/env bash
# Compila a app do iPad SEM assinatura e cria um .ipa (num Mac ou no runner macOS do GitHub Actions).
#
# Um .ipa sem assinatura não se instala diretamente: é assinado no momento da instalação, a partir
# do Windows, com o Sideloadly e um Apple ID (gratuito serve). Ver docs/SPIKES.md §2.
#
# Uso:   bash ipad-sender/scripts/build-ipa.sh [pasta-de-saída]     (por omissão ipad-sender/build/ipa)
# Variáveis opcionais:
#   PADSTUDIO_BUNDLE_PREFIX  prefixo dos bundle IDs (por omissão o de Config/Shared.xcconfig)
#   BUILD_NUMBER             número de build (CFBundleVersion), ex.: o número da execução do CI
#   CONFIGURATION            Release (por omissão) ou Debug
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="${1:-$root/build/ipa}"
configuration="${CONFIGURATION:-Release}"
derived="$root/build/DerivedData"

fail() {
  echo "erro: $*" >&2
  exit 1
}

command -v xcodegen >/dev/null || fail "falta o XcodeGen (brew install xcodegen)"
command -v xcodebuild >/dev/null || fail "falta o Xcode"

settings=(CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO CODE_SIGN_IDENTITY= DEVELOPMENT_TEAM=)
if [[ -n "${PADSTUDIO_BUNDLE_PREFIX:-}" ]]; then
  [[ "$PADSTUDIO_BUNDLE_PREFIX" =~ ^[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)+$ ]] \
    || fail "PADSTUDIO_BUNDLE_PREFIX inválido: '$PADSTUDIO_BUNDLE_PREFIX' (ex.: com.o-teu-nome.padstudio)"
  settings+=("PADSTUDIO_BUNDLE_PREFIX=$PADSTUDIO_BUNDLE_PREFIX")
fi
if [[ -n "${BUILD_NUMBER:-}" ]]; then
  [[ "$BUILD_NUMBER" =~ ^[0-9]+$ ]] || fail "BUILD_NUMBER tem de ser um número"
  settings+=("CURRENT_PROJECT_VERSION=$BUILD_NUMBER")
fi

echo "==> XcodeGen"
cd "$root"
xcodegen generate --quiet

echo "==> xcodebuild ($configuration, iphoneos, sem assinatura)"
mkdir -p "$out"
rm -rf "$derived/Build/Products/$configuration-iphoneos"
xcodebuild \
  -project PadStudio.xcodeproj \
  -scheme PadStudioHost \
  -configuration "$configuration" \
  -sdk iphoneos \
  -destination 'generic/platform=iOS' \
  -derivedDataPath "$derived" \
  -quiet \
  "${settings[@]}" \
  build

app="$derived/Build/Products/$configuration-iphoneos/PadStudioHost.app"
appex="$app/PlugIns/PadStudioBroadcast.appex"
[[ -d "$app" ]] || fail "não encontrei $app"
[[ -d "$appex" ]] || fail "a extensão PadStudioBroadcast não foi embebida na app"

plist() { /usr/libexec/PlistBuddy -c "Print :$2" "$1/Info.plist" 2>/dev/null || echo "?"; }
point="$(plist "$appex" NSExtension:NSExtensionPointIdentifier)"
[[ "$point" == "com.apple.broadcast-services-upload" ]] || fail "extensão com NSExtensionPointIdentifier inesperado: $point"
app_id="$(plist "$app" CFBundleIdentifier)"
ext_id="$(plist "$appex" CFBundleIdentifier)"
[[ "$ext_id" == "$app_id."* ]] || fail "o bundle ID da extensão ($ext_id) tem de começar por $app_id."

echo "==> .ipa"
staging="$(mktemp -d)"
trap 'rm -rf "$staging"' EXIT
mkdir "$staging/Payload"
cp -R "$app" "$staging/Payload/"
ipa="$out/PadStudio-unsigned.ipa"
rm -f "$ipa"
(cd "$staging" && zip -qry "$ipa" Payload)

{
  echo "PadStudio $(plist "$app" CFBundleShortVersionString) (build $(plist "$app" CFBundleVersion)) — SEM assinatura"
  echo "App:      $app_id"
  echo "Extensão: $ext_id ($point)"
  echo "iOS mínimo: $(plist "$app" MinimumOSVersion)"
  echo "Arquiteturas: $(lipo -archs "$app/PadStudioHost" 2>/dev/null || echo '?')"
  echo "$(xcodebuild -version | tr '\n' ' ')"
  echo "Commit: ${GITHUB_SHA:-$(git -C "$root" rev-parse HEAD 2>/dev/null || echo '?')}"
  echo "Data: $(date -u '+%Y-%m-%d %H:%M UTC')"
  echo "SHA-256: $(shasum -a 256 "$ipa" | cut -d' ' -f1)"
  echo "Tamanho: $(du -h "$ipa" | cut -f1)"
} > "$out/BUILD_INFO.txt"
cat "$out/BUILD_INFO.txt"
