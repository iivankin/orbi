use std::path::Path;

use super::write_executable;

const IDB_MOCK_SCRIPT: &str = r#"#!/bin/sh
set -eu
echo "idb $@" >> "$MOCK_LOG"
cmd=""
if [ "$#" -ge 2 ] && [ "$1" = "ui" ]; then
  cmd="$2"
elif [ "$#" -ge 1 ]; then
  cmd="$1"
fi
case "$cmd" in
  describe-all)
    cat <<'JSON'
[
  {
    "AXLabel": "ExampleApp",
    "frame": { "x": 0, "y": 0, "width": 393, "height": 852 }
  },
  {
    "AXLabel": "Continue",
    "frame": { "x": 40, "y": 120, "width": 200, "height": 44 }
  },
  {
    "AXIdentifier": "email-value",
    "AXLabel": "qa@example.com",
    "frame": { "x": 40, "y": 180, "width": 200, "height": 44 }
  },
  {
    "AXLabel": "Welcome",
    "frame": { "x": 40, "y": 200, "width": 200, "height": 44 }
  }
]
JSON
    ;;
  describe-point)
    cat <<'JSON'
{
  "AXLabel": "Continue",
  "frame": { "x": 40, "y": 120, "width": 200, "height": 44 }
}
JSON
    ;;
  video|record-video)
    out="$2"
    mkdir -p "$(dirname "$out")"
    printf 'mp4' > "$out"
    ;;
  log)
    printf 'mock log line\n'
    ;;
  crash)
    sub="$2"
    case "$sub" in
      list)
        printf 'mock-crash-1.ips\n'
        ;;
      show)
        printf 'mock crash payload\n'
        ;;
      delete)
        ;;
      *)
        echo "unexpected idb crash command: $@" >&2
        exit 1
        ;;
    esac
    ;;
  contacts)
    if [ "$#" -ge 2 ] && [ "$2" = "update" ]; then
      :
    else
      echo "unexpected idb contacts command: $@" >&2
      exit 1
    fi
    ;;
  dylib)
    if [ "$#" -ge 2 ] && [ "$2" = "install" ]; then
      :
    else
      echo "unexpected idb dylib command: $@" >&2
      exit 1
    fi
    ;;
  instruments)
    printf 'mock instruments trace\n'
    ;;
  tap|text|swipe|clear-keychain|set-location|uninstall|approve|launch|focus|add-media|kill|open)
    ;;
  button|key|key-sequence)
    ;;
  *)
    echo "unexpected idb command: $@" >&2
    exit 1
    ;;
esac
"#;

const IDB_COMPANION_MOCK_SCRIPT: &str = r#"#!/bin/sh
set -eu
echo "idb_companion $@" >> "$MOCK_LOG"
"#;

pub fn create_security_mock(mock_bin: &Path, db_path: &Path) {
    write_executable(
        &mock_bin.join("security"),
        &format!(
            r#"#!/bin/sh
set -eu
echo "security $@" >> "$MOCK_LOG"
db="{db}"
cmd="$1"
shift
case "$cmd" in
  find-generic-password)
    account=""
    service=""
    while [ "$#" -gt 0 ]; do
      case "$1" in
        -a) account="$2"; shift 2 ;;
        -s) service="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    value="$(awk -F'|' -v svc="$service" -v acct="$account" '$1 == svc && $2 == acct {{ print $3; exit }}' "$db" 2>/dev/null)"
    if [ -z "$value" ]; then
      exit 44
    fi
    printf '%s\n' "$value"
    ;;
  delete-generic-password)
    account=""
    service=""
    while [ "$#" -gt 0 ]; do
      case "$1" in
        -a) account="$2"; shift 2 ;;
        -s) service="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    tmp="$db.tmp"
    touch "$db"
    grep -v "^$service|$account|" "$db" > "$tmp" || true
    mv "$tmp" "$db"
    ;;
  list-keychains)
    if [ "$#" -ge 2 ] && [ "$1" = "-d" ] && [ "$2" = "user" ]; then
      printf '"%s/Library/Keychains/login.keychain-db"\n' "$HOME"
      exit 0
    fi
    ;;
  create-keychain|unlock-keychain|set-keychain-settings|set-key-partition-list)
    ;;
  import)
    p12=""
    keychain=""
    password=""
    cert_path=""
    cert_format=""
    while [ "$#" -gt 0 ]; do
      case "$1" in
        -k) keychain="$2"; shift 2 ;;
        -P) password="$2"; shift 2 ;;
        -T) shift 2 ;;
        *)
          if [ -z "$p12" ]; then
            p12="$1"
          fi
          shift
          ;;
      esac
    done
    mkdir -p "$(dirname "$db")"
    touch "$db"
    der_path="${{p12%.*}}.cer"
    pem_path="${{p12%.*}}.pem"
    if [ -f "$der_path" ]; then
      cert_path="$der_path"
      cert_format="DER"
    elif [ -f "$pem_path" ]; then
      cert_path="$pem_path"
      cert_format="PEM"
    fi
    if [ -n "$cert_path" ]; then
      if [ "$cert_format" = "DER" ]; then
        hash="$(openssl x509 -inform DER -in "$cert_path" -noout -fingerprint -sha1 | sed 's/.*=//; s/://g')"
        name="$(openssl x509 -inform DER -in "$cert_path" -noout -subject | sed -E 's/^subject= *//; s/.*CN *= *//')"
      else
        hash="$(openssl x509 -in "$cert_path" -noout -fingerprint -sha1 | sed 's/.*=//; s/://g')"
        name="$(openssl x509 -in "$cert_path" -noout -subject | sed -E 's/^subject= *//; s/.*CN *= *//')"
      fi
    else
      cert_tmp="$(mktemp)"
      if openssl pkcs12 -in "$p12" -clcerts -nokeys -passin "pass:$password" -out "$cert_tmp" >/dev/null 2>&1; then
        hash="$(openssl x509 -in "$cert_tmp" -noout -fingerprint -sha1 | sed 's/.*=//; s/://g')"
        name="$(openssl x509 -in "$cert_tmp" -noout -subject | sed -E 's/^subject= *//; s/.*CN *= *//')"
        cert_path="$(dirname "$db")/$hash.pem"
        cp "$cert_tmp" "$cert_path"
        cert_format="PEM"
        rm -f "$cert_tmp"
      else
        rm -f "$cert_tmp"
        hash="$(printf '%s' "$p12" | shasum | awk '{{print toupper(substr($1, 1, 40))}}')"
        name="Imported Identity"
      fi
    fi
    tmp="$db.tmp"
    grep -v "^import|$keychain|$hash|" "$db" > "$tmp" || true
    printf '%s|%s|%s|%s|%s|%s|%s|%s\n' "import" "$keychain" "$hash" "$name" "$cert_path" "$cert_format" "$p12" "$password" >> "$tmp"
    mv "$tmp" "$db"
    ;;
  find-identity)
    keychain=""
    while [ "$#" -gt 0 ]; do
      case "$1" in
        -p|-s) shift 2 ;;
        -v) shift ;;
        *)
          keychain="$1"
          shift
          ;;
      esac
    done
    touch "$db"
    awk -F'|' -v kc="$keychain" '
      $1 == "import" && (kc == "" || $2 == kc) {{
        count += 1
        printf "  %d) %s \"%s\"\n", count, $3, $4
      }}
    ' "$db"
    ;;
  find-certificate)
    keychain=""
    while [ "$#" -gt 0 ]; do
      case "$1" in
        -a|-Z|-p) shift ;;
        *)
          keychain="$1"
          shift
          ;;
      esac
    done
    touch "$db"
    awk -F'|' -v kc="$keychain" '
      $1 == "import" && (kc == "" || $2 == kc) {{
        printf "%s|%s|%s|%s|%s\n", $3, $5, $6, $7, $8
      }}
    ' "$db" | while IFS='|' read -r hash cert_path cert_format p12 password; do
      [ -n "$hash" ] || continue
      printf 'SHA-1 hash: %s\n' "$hash"
      if [ -n "$cert_path" ] && [ -f "$cert_path" ]; then
        if [ "$cert_format" = "DER" ]; then
          openssl x509 -inform DER -in "$cert_path" -outform PEM 2>/dev/null
        else
          openssl x509 -in "$cert_path" -outform PEM 2>/dev/null
        fi
      elif [ -n "$p12" ] && [ -f "$p12" ]; then
        openssl pkcs12 -in "$p12" -clcerts -nokeys -passin "pass:$password" 2>/dev/null
      fi
    done
    exit 0
    ;;
  *)
    echo "unexpected security command: $cmd" >&2
    exit 1
    ;;
esac
"#,
            db = db_path.display()
        ),
    );
}

pub fn create_watch_xcrun_mock(mock_bin: &Path, sdk_root: &Path) {
    create_xcrun_mock(mock_bin, sdk_root, XcrunMockKind::Watch);
}

pub fn create_lldb_attach_mock(developer_dir: &Path) {
    let bin_dir = developer_dir.join("usr").join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    write_executable(
        &bin_dir.join("lldb"),
        r#"#!/bin/sh
set -eu
echo "lldb $@" >> "$MOCK_LOG"
printf '(lldb) '
while IFS= read -r line; do
  echo "$line" >> "$MOCK_LOG"
  case "$line" in
    "process attach -i -w -n "*)
      printf 'Process 123 stopped\n'
      printf '(lldb) '
      ;;
    "process continue")
      printf 'Process 123 resuming\n'
      printf '(lldb) '
      exit 0
      ;;
    *)
      printf '(lldb) '
      ;;
  esac
done
"#,
    );
}

pub fn create_xcodebuild_mock(mock_bin: &Path) {
    write_executable(
        &mock_bin.join("xcodebuild"),
        r#"#!/bin/sh
set -eu
echo "xcodebuild $@" >> "$MOCK_LOG"
if [ "$#" -eq 1 ] && [ "$1" = "-version" ]; then
  printf '%s\n' "Xcode 16.0"
  printf '%s\n' "Build version 16A242d"
  exit 0
fi
echo "unexpected xcodebuild command: $@" >&2
exit 1
"#,
    );
}

pub fn create_sw_vers_mock(mock_bin: &Path) {
    write_executable(
        &mock_bin.join("sw_vers"),
        r#"#!/bin/sh
set -eu
echo "sw_vers $@" >> "$MOCK_LOG"
if [ "$#" -ne 1 ]; then
  echo "unexpected sw_vers command: $@" >&2
  exit 1
fi
case "$1" in
  -productVersion)
    printf '%s\n' "15.0"
    ;;
  -buildVersion)
    printf '%s\n' "24A335"
    ;;
  *)
    echo "unexpected sw_vers command: $@" >&2
    exit 1
    ;;
esac
"#,
    );
}

pub fn create_build_xcrun_mock(mock_bin: &Path, sdk_root: &Path) {
    create_xcrun_mock(mock_bin, sdk_root, XcrunMockKind::Build);
}

pub fn create_quality_swift_mock(mock_bin: &Path) {
    write_executable(
        &mock_bin.join("swift"),
        r#"#!/bin/sh
set -eu
echo "swift $@" >> "$MOCK_LOG"
if [ "$#" -ge 4 ] && [ "$1" = "package" ] && [ "$2" = "--package-path" ] && [ "$4" = "dump-package" ]; then
  package_path="$3"
  if [ -f "$package_path/Sources/OrbiPkg/OrbiPkg.swift" ]; then
    cat <<'JSON'
{"name":"OrbiPkg","products":[{"name":"OrbiPkg","targets":["OrbiPkg"]}],"targets":[{"name":"OrbiPkg","path":"Sources/OrbiPkg","dependencies":[],"type":"regular"}]}
JSON
    exit 0
  fi
  echo "unexpected package path: $package_path" >&2
  exit 1
fi
scratch=""
product=""
show_bin_path=0
prev=""
for arg in "$@"; do
  if [ "$prev" = "--scratch-path" ]; then
    scratch="$arg"
  fi
  if [ "$prev" = "--product" ]; then
    product="$arg"
  fi
  if [ "$arg" = "--show-bin-path" ]; then
    show_bin_path=1
  fi
  prev="$arg"
done
if [ -z "$scratch" ]; then
  echo "missing --scratch-path" >&2
  exit 1
fi
bin_dir="$scratch/release"
mkdir -p "$bin_dir"
if [ "$show_bin_path" -eq 1 ]; then
  printf '%s\n' "$bin_dir"
  exit 0
fi
case "$product" in
  orbi-swift-format|orbi-swiftlint)
    cat > "$bin_dir/$product" <<'SCRIPT'
#!/bin/sh
set -eu
echo "__PRODUCT__ $@" >> "$MOCK_LOG"
printf '%s\n' "__PRODUCT__ request:" >> "$MOCK_LOG"
cat "$1" >> "$MOCK_LOG"
printf '\n' >> "$MOCK_LOG"
SCRIPT
    sed -i '' "s#__PRODUCT__#$product#g" "$bin_dir/$product"
    chmod +x "$bin_dir/$product"
    exit 0
    ;;
  *)
    echo "unexpected swift product: $product" >&2
    exit 1
    ;;
esac
"#,
    );
}

pub fn create_testing_swift_mock(mock_bin: &Path) {
    write_executable(
        &mock_bin.join("swift"),
        r#"#!/bin/sh
set -eu
echo "swift $@" >> "$MOCK_LOG"
if [ "$#" -lt 1 ] || [ "$1" != "test" ]; then
  echo "unexpected swift command: $@" >&2
  exit 1
fi
package_path=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "--package-path" ]; then
    package_path="$arg"
  fi
  prev="$arg"
done
if [ -z "$package_path" ] || [ ! -f "$package_path/Package.swift" ]; then
  echo "missing generated Package.swift" >&2
  exit 1
fi
if [ -n "${ORBI_TRACE_OUTPUT:-}" ] && [ "${MOCK_ORBI_TRACE_SKIP_OUTPUT:-0}" != "1" ]; then
  mkdir -p "$(dirname "$ORBI_TRACE_OUTPUT")"
  case "${ORBI_TRACE_MODE:-cpu}" in
    memory|allocations)
      cat > "$ORBI_TRACE_OUTPUT" <<'JSON'
{"format":"orbi.trace.v1","formatVersion":1,"mode":"memory","startedAtUnixNanos":1,"host":"mock","memory":{"summary":{"totalAllocatedBytes":128,"allocationEvents":2,"liveBytes":64,"liveAllocations":1,"peakLiveBytes":128},"dropped":{"allocationRecords":0,"allocationStacks":0,"processSamples":0,"unknownFrees":0},"processMemorySamples":[],"stacks":[{"stack":["0x1000"],"totalAllocatedBytes":128,"allocationCount":2,"liveBytes":64,"liveAllocations":1,"peakLiveBytes":128}]}}
JSON
      ;;
    *)
      cat > "$ORBI_TRACE_OUTPUT" <<'JSON'
{"format":"orbi.trace.v1","formatVersion":1,"mode":"cpu","startedAtUnixNanos":1,"host":"mock","cpu":{"sampleIntervalMicros":4500,"droppedSamples":0,"failedUnwinds":0,"processMemorySamples":[],"threads":[{"id":1,"name":"main","samples":[{"timeNanos":1,"stack":["0x1000"]}]}]}}
JSON
      ;;
  esac
fi
"#,
    );
}

pub fn create_idb_mock(mock_bin: &Path) {
    write_executable(&mock_bin.join("idb"), IDB_MOCK_SCRIPT);
    write_executable(&mock_bin.join("idb_companion"), IDB_COMPANION_MOCK_SCRIPT);
}

pub fn create_macos_ui_helper_mock(mock_bin: &Path) -> std::path::PathBuf {
    let path = mock_bin.join("orbi-macos-ui-helper");
    write_executable(
        &path,
        r#"#!/usr/bin/env python3
import json
import os
import pathlib
import sys

LOG = pathlib.Path(os.environ["MOCK_LOG"])

TREE = {
    "AXRole": "AXApplication",
    "AXLabel": "ExampleApp",
    "frame": {"x": 0, "y": 0, "width": 800, "height": 600},
    "children": [
        {
            "AXRole": "AXWindow",
            "AXLabel": "Example Window",
            "frame": {"x": 20, "y": 40, "width": 500, "height": 360},
            "children": [
                {
                    "AXRole": "AXButton",
                    "AXLabel": "Continue",
                    "AXIdentifier": "continue-button",
                    "frame": {"x": 40, "y": 120, "width": 200, "height": 44},
                    "actions": ["AXPress"]
                },
                {
                    "AXRole": "AXStaticText",
                    "AXLabel": "qa@example.com",
                    "AXIdentifier": "email-value",
                    "AXValue": "qa@example.com",
                    "frame": {"x": 40, "y": 180, "width": 200, "height": 44}
                }
            ]
        }
    ]
}
ACTIVE_RECORDING_PATH = None

def log(command, params):
    with LOG.open("a", encoding="utf-8") as handle:
        handle.write("macos-helper " + command + " " + json.dumps(params, sort_keys=True) + "\n")

def ok(request, result=None):
    return {"id": request["id"], "ok": True, "result": result if result is not None else {}, "error": None}

def fail(request, message):
    return {"id": request.get("id", -1), "ok": False, "result": None, "error": message}

def write_trace_if_requested(params):
    if os.environ.get("MOCK_ORBI_TRACE_SKIP_OUTPUT") == "1":
        return
    env = {}
    for item in params.get("environment") or []:
        key = item.get("key")
        value = item.get("value")
        if isinstance(key, str) and isinstance(value, str):
            env[key] = value
    output = env.get("ORBI_TRACE_OUTPUT")
    if not output:
        return
    path = pathlib.Path(output)
    path.parent.mkdir(parents=True, exist_ok=True)
    if env.get("ORBI_TRACE_MODE") in {"memory", "allocations"}:
        path.write_text('{"format":"orbi.trace.v1","formatVersion":1,"mode":"memory","startedAtUnixNanos":1,"host":"mock","memory":{"summary":{"totalAllocatedBytes":128,"allocationEvents":2,"liveBytes":64,"liveAllocations":1,"peakLiveBytes":128},"dropped":{"allocationRecords":0,"allocationStacks":0,"processSamples":0,"unknownFrees":0},"processMemorySamples":[],"stacks":[{"stack":["0x1000"],"totalAllocatedBytes":128,"allocationCount":2,"liveBytes":64,"liveAllocationCount":1,"peakLiveBytes":128}]}}')
    else:
        path.write_text('{"format":"orbi.trace.v1","formatVersion":1,"mode":"cpu","startedAtUnixNanos":1,"host":"mock","cpu":{"sampleIntervalMicros":4500,"droppedSamples":0,"failedUnwinds":0,"processMemorySamples":[],"threads":[{"id":1,"name":"main","samples":[{"timeNanos":1,"stack":["0x1000"]}]}]}}')

for line in sys.stdin:
    try:
        request = json.loads(line)
        command = request["command"]
        params = request.get("params") or {}
        log(command, params)
        if command == "checkPermissions":
            response = ok(request, {
                "backendAvailable": True,
                "accessibilityTrusted": True,
                "screenCaptureAccess": True,
            })
        elif command == "launchApp":
            write_trace_if_requested(params)
            response = ok(request)
        elif command == "waitForApp":
            response = ok(request)
        elif command in {"stopApp", "clearAppState", "focus", "inputText", "pressKey", "pressKeyCode", "pressKeySequence", "selectMenuItem", "scroll", "swipe", "drag", "hoverPoint", "rightClickPoint", "tapPoint"}:
            response = ok(request)
        elif command == "describeAll":
            response = ok(request, TREE)
        elif command == "describePoint":
            response = ok(request, TREE["children"][0]["children"][0])
        elif command == "activateSelector":
            response = ok(request, True)
        elif command == "takeScreenshot":
            path = pathlib.Path(params["path"])
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"png")
            response = ok(request)
        elif command == "startVideoRecording":
            path = pathlib.Path(params["path"])
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"mp4")
            ACTIVE_RECORDING_PATH = path
            response = ok(request)
        elif command == "stopVideoRecording":
            ACTIVE_RECORDING_PATH = None
            response = ok(request)
        else:
            response = fail(request, "unexpected macOS helper command: " + command)
    except Exception as error:
        response = fail({"id": -1}, str(error))
    print(json.dumps(response), flush=True)
"#,
    );
    write_executable(
        &mock_bin.join("open"),
        r#"#!/usr/bin/env python3
import json
import os
import pathlib
import sys

LOG = pathlib.Path(os.environ["MOCK_LOG"])

def log(message):
    with LOG.open("a", encoding="utf-8") as handle:
        handle.write(message + "\n")

def write_trace(env):
    output = env.get("ORBI_TRACE_OUTPUT")
    if not output or os.environ.get("MOCK_ORBI_TRACE_SKIP_OUTPUT") == "1":
        return
    path = pathlib.Path(output)
    path.parent.mkdir(parents=True, exist_ok=True)
    if env.get("ORBI_TRACE_MODE") in {"memory", "allocations"}:
        path.write_text('{"format":"orbi.trace.v1","formatVersion":1,"mode":"memory","startedAtUnixNanos":1,"host":"mock","memory":{"summary":{"totalAllocatedBytes":128,"allocationEvents":2,"liveBytes":64,"liveAllocations":1,"peakLiveBytes":128},"dropped":{"allocationRecords":0,"allocationStacks":0,"processSamples":0,"unknownFrees":0},"processMemorySamples":[],"stacks":[{"stack":["0x1000"],"totalAllocatedBytes":128,"allocationCount":2,"liveBytes":64,"liveAllocationCount":1,"peakLiveBytes":128}]}}')
    else:
        path.write_text('{"format":"orbi.trace.v1","formatVersion":1,"mode":"cpu","startedAtUnixNanos":1,"host":"mock","cpu":{"sampleIntervalMicros":4500,"droppedSamples":0,"failedUnwinds":0,"processMemorySamples":[],"threads":[{"id":1,"name":"main","samples":[{"timeNanos":1,"stack":["0x1000"]}]}]}}')

args = sys.argv[1:]
env = {}
stdout_path = None
stderr_path = None
index = 0
while index < len(args):
    arg = args[index]
    if arg == "--env" and index + 1 < len(args):
        raw = args[index + 1]
        if "=" in raw:
            key, value = raw.split("=", 1)
            env[key] = value
        index += 2
        continue
    if arg == "--stdout" and index + 1 < len(args):
        stdout_path = args[index + 1]
        index += 2
        continue
    if arg == "--stderr" and index + 1 < len(args):
        stderr_path = args[index + 1]
        index += 2
        continue
    index += 1

log("open " + json.dumps({"args": args, "env": env}, sort_keys=True))
write_trace(env)
for path, line in [(stdout_path, "ExampleMacApp print launched\n"), (stderr_path, "ExampleMacApp launched\n")]:
    if path:
        with open(path, "a", encoding="utf-8") as handle:
            handle.write(line)
"#,
    );
    path
}

pub fn create_python3_fb_idb_install_mock(mock_bin: &Path) {
    write_executable(
        &mock_bin.join("python3"),
        &format!(
            r#"#!/bin/sh
set -eu
echo "python3 $@" >> "$MOCK_LOG"
if [ "$#" -ge 6 ] && [ "$1" = "-m" ] && [ "$2" = "pip" ] && [ "$3" = "install" ]; then
  case " $* " in
    *" fb-idb==1.1.7 "*)
      bin_dir="$HOME/Library/Python/3.12/bin"
      mkdir -p "$bin_dir"
      cat > "$bin_dir/idb" <<'EOF'
{idb_script}
EOF
      chmod +x "$bin_dir/idb"
      exit 0
      ;;
  esac
fi
echo "unexpected python3 command: $@" >&2
exit 1
"#,
            idb_script = IDB_MOCK_SCRIPT
        ),
    );
}

pub fn create_brew_idb_companion_install_mock(mock_bin: &Path) {
    write_executable(
        &mock_bin.join("brew"),
        &format!(
            r#"#!/bin/sh
set -eu
echo "brew $@" >> "$MOCK_LOG"
prefix="$HOME/.orbi-test-brew/idb-companion"
cmd="${{1:-}}"
case "$cmd" in
  tap)
    exit 0
    ;;
  install)
    if [ "$#" -eq 2 ] && [ "$2" = "idb-companion" ]; then
      mkdir -p "$prefix/bin"
      cat > "$prefix/bin/idb_companion" <<'EOF'
{companion_script}
EOF
      chmod +x "$prefix/bin/idb_companion"
      exit 0
    fi
    ;;
  --prefix)
    if [ "$#" -eq 2 ] && [ "$2" = "idb-companion" ] && [ -x "$prefix/bin/idb_companion" ]; then
      printf '%s\n' "$prefix"
      exit 0
    fi
    exit 1
    ;;
esac
echo "unexpected brew command: $@" >&2
exit 1
"#,
            companion_script = IDB_COMPANION_MOCK_SCRIPT
        ),
    );
}

pub fn create_ditto_mock(mock_bin: &Path) {
    write_executable(
        &mock_bin.join("ditto"),
        r#"#!/bin/sh
set -eu
echo "ditto $@" >> "$MOCK_LOG"
if [ "$#" -lt 2 ]; then
  echo "ditto mock expects at least source and destination" >&2
  exit 1
fi
src=""
out=""
prev=""
for arg in "$@"; do
  src="$prev"
  out="$arg"
  prev="$arg"
done
mkdir -p "$(dirname "$out")"
rm -f "$out"
src_parent="$(dirname "$src")"
src_name="$(basename "$src")"
(
  cd "$src_parent"
  /usr/bin/zip -qry "$out" "$src_name"
)
"#,
    );
}

pub fn create_codesign_mock(mock_bin: &Path) {
    write_executable(
        &mock_bin.join("codesign"),
        r#"#!/bin/sh
set -eu
echo "codesign $@" >> "$MOCK_LOG"
if [ "$#" -lt 1 ]; then
  echo "codesign mock expects a bundle path" >&2
  exit 1
fi
bundle=""
verify=0
for arg in "$@"; do
  case "$arg" in
    -dv|--display|--verbose=*)
      verify=1
      ;;
  esac
  bundle="$arg"
done
if [ "$verify" -eq 1 ]; then
  if [ -d "$bundle" ]; then
    printf 'Executable=%s/Contents/MacOS/ExampleApp\n' "$bundle" >&2
    printf 'flags=0x10000(runtime)\n' >&2
  fi
  printf 'Authority=Developer ID Application: Example Team\n' >&2
  exit 0
fi
if [ -d "$bundle/Contents" ]; then
  signature_root="$bundle/Contents/_CodeSignature"
elif [ -d "$bundle" ]; then
  signature_root="$bundle/_CodeSignature"
else
  mkdir -p "$(dirname "$bundle")"
  printf 'signed\n' > "$bundle.signature"
  exit 0
fi
mkdir -p "$signature_root"
printf 'signed\n' > "$signature_root/CodeResources"
"#,
    );
}

pub fn create_hdiutil_mock(mock_bin: &Path) {
    write_executable(
        &mock_bin.join("hdiutil"),
        r#"#!/bin/sh
set -eu
echo "hdiutil $@" >> "$MOCK_LOG"
if [ "$#" -lt 1 ]; then
  echo "hdiutil mock expects an output path" >&2
  exit 1
fi
out=""
for arg in "$@"; do
  out="$arg"
done
mkdir -p "$(dirname "$out")"
printf 'dmg' > "$out"
"#,
    );
}

pub fn create_passthrough_mock(mock_bin: &Path, name: &str) {
    write_executable(
        &mock_bin.join(name),
        &format!(
            r#"#!/bin/sh
set -eu
echo "{name} $@" >> "$MOCK_LOG"
"#,
        ),
    );
}

enum XcrunMockKind {
    Build,
    Watch,
}

fn create_xcrun_mock(mock_bin: &Path, sdk_root: &Path, kind: XcrunMockKind) {
    let sdk_version_block = match kind {
        XcrunMockKind::Build => "  printf '%s\\n' \"18.0\"\n  exit 0",
        XcrunMockKind::Watch => {
            "  case \"$2\" in\n    watchos|watchsimulator) printf '%s\\n' \"11.0\" ;;\n    *) printf '%s\\n' \"18.0\" ;;\n  esac\n  exit 0"
        }
    };
    let extra_commands = match kind {
        XcrunMockKind::Build => {
            r#"if [ "$1" = "altool" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "list" ] && [ "$3" = "devices" ]; then
  cat <<'JSON'
{"devices":{"com.apple.CoreSimulator.SimRuntime.iOS-18-0":[{"udid":"IOS-UDID","name":"iPhone 16","state":"Booted"}]}}
JSON
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "boot" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "bootstatus" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "install" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "get_app_container" ]; then
  container="$(dirname "$MOCK_LOG")/simulator-containers/$3/$4/data"
  mkdir -p "$container/Documents"
  printf '%s\n' "$container"
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "launch" ]; then
  udid=""
  bundle=""
  for arg in "$@"; do
    case "$arg" in
      simctl|launch|--console-pty|--terminate-running-process|--wait-for-debugger)
        ;;
      -*)
        ;;
      *)
        if [ -z "$udid" ]; then
          udid="$arg"
        elif [ -z "$bundle" ]; then
          bundle="$arg"
          break
        fi
        ;;
    esac
  done
  if [ -n "$udid" ] && [ -n "$bundle" ] && [ "${MOCK_ORBI_TRACE_SKIP_OUTPUT:-0}" != "1" ]; then
    trace_root="$(dirname "$MOCK_LOG")/simulator-containers/$udid/$bundle/data/Documents"
    mkdir -p "$trace_root"
    cat > "$trace_root/orbi-trace.json" <<'JSON'
{"format":"orbi.trace.v1","formatVersion":1,"mode":"memory","startedAtUnixNanos":1,"host":"mock","memory":{"summary":{"totalAllocatedBytes":128,"allocationEvents":2,"liveBytes":64,"liveAllocations":1,"peakLiveBytes":128},"dropped":{"allocationRecords":0,"allocationStacks":0,"processSamples":0,"unknownFrees":0},"processMemorySamples":[],"stacks":[{"stack":["0x1000"],"totalAllocatedBytes":128,"allocationCount":2,"liveBytes":64,"liveAllocations":1,"peakLiveBytes":128}]}}
JSON
  fi
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "spawn" ] && [ "$4" = "log" ] && [ "$5" = "stream" ]; then
  process_name=""
  prev=""
  for arg in "$@"; do
    if [ "$prev" = "--process" ]; then
      process_name="$arg"
    fi
    prev="$arg"
  done
  printf '%s\n' "Filtering the log data using \"process == $process_name\""
  printf '2026-04-02 12:00:00.000000+0000 %s[123:456] mock log line\n' "$process_name"
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "terminate" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "openurl" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "privacy" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "location" ] && [ "$4" = "start" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "io" ] && [ "$4" = "screenshot" ]; then
  mkdir -p "$(dirname "$5")"
  printf 'png' > "$5"
  exit 0
fi"#
        }
        XcrunMockKind::Watch => {
            r#"if [ "$1" = "simctl" ] && [ "$2" = "list" ] && [ "$3" = "devices" ]; then
  cat <<'JSON'
{"devices":{"com.apple.CoreSimulator.SimRuntime.watchOS-11-0":[{"udid":"WATCH-UDID","name":"Apple Watch Series 9","state":"Shutdown"}]}}
JSON
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "boot" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "bootstatus" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "install" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "get_app_container" ]; then
  container="$(dirname "$MOCK_LOG")/simulator-containers/$3/$4/data"
  mkdir -p "$container/Documents"
  printf '%s\n' "$container"
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "launch" ]; then
  udid=""
  bundle=""
  for arg in "$@"; do
    case "$arg" in
      simctl|launch|--console-pty|--terminate-running-process|--wait-for-debugger)
        ;;
      -*)
        ;;
      *)
        if [ -z "$udid" ]; then
          udid="$arg"
        elif [ -z "$bundle" ]; then
          bundle="$arg"
          break
        fi
        ;;
    esac
  done
  if [ -n "$udid" ] && [ -n "$bundle" ] && [ "${MOCK_ORBI_TRACE_SKIP_OUTPUT:-0}" != "1" ]; then
    trace_root="$(dirname "$MOCK_LOG")/simulator-containers/$udid/$bundle/data/Documents"
    mkdir -p "$trace_root"
    cat > "$trace_root/orbi-trace.json" <<'JSON'
{"format":"orbi.trace.v1","formatVersion":1,"mode":"cpu","startedAtUnixNanos":1,"host":"mock","cpu":{"sampleIntervalMicros":4500,"droppedSamples":0,"failedUnwinds":0,"processMemorySamples":[],"threads":[{"id":1,"name":"main","samples":[{"timeNanos":1,"stack":["0x1000"]}]}]}}
JSON
  fi
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "terminate" ]; then
  exit 0
fi"#
        }
    };
    write_executable(
        &mock_bin.join("xcrun"),
        &format!(
            r#"#!/bin/sh
set -eu
echo "xcrun $@" >> "$MOCK_LOG"
if [ "$#" -ge 3 ] && [ "$1" = "--sdk" ] && [ "$3" = "--show-sdk-path" ]; then
  mkdir -p "{sdk}"
  printf '%s\n' "{sdk}"
  exit 0
fi
if [ "$#" -ge 2 ] && [ "$1" = "--find" ] && [ "$2" = "swiftc" ]; then
  printf '%s\n' "{sdk}/Toolchains/OrbiDefault.xctoolchain/usr/bin/swiftc"
  exit 0
fi
if [ "$#" -ge 3 ] && [ "$1" = "--sdk" ] && [ "$3" = "--show-sdk-version" ]; then
{sdk_version_block}
fi
if [ "$#" -ge 3 ] && [ "$1" = "--sdk" ] && [ "$3" = "--show-sdk-build-version" ]; then
  printf '%s\n' "TESTSDK1"
  exit 0
fi
if [ "$#" -ge 3 ] && [ "$1" = "--sdk" ] && [ "$3" = "swiftc" ]; then
  out=""
  module=""
  prev=""
  for arg in "$@"; do
    if [ "$prev" = "-o" ]; then
      out="$arg"
    fi
    if [ "$prev" = "-emit-module-path" ]; then
      module="$arg"
    fi
    prev="$arg"
  done
  mkdir -p "$(dirname "$out")"
  cat > "$out" <<'SCRIPT'
#!/bin/sh
set -eu
echo "$(basename "$0") $@" >> "$MOCK_LOG"
printf '%s\n' "$$" > "$(dirname "$MOCK_LOG")/macos-ui-application.pid"
if [ -n "${{ORBI_TRACE_OUTPUT:-}}" ] && [ "${{MOCK_ORBI_TRACE_SKIP_OUTPUT:-0}}" != "1" ]; then
  mkdir -p "$(dirname "$ORBI_TRACE_OUTPUT")"
  case "${{ORBI_TRACE_MODE:-cpu}}" in
    memory|allocations)
      cat > "$ORBI_TRACE_OUTPUT" <<'JSON'
{{"format":"orbi.trace.v1","formatVersion":1,"mode":"memory","startedAtUnixNanos":1,"host":"mock","memory":{{"summary":{{"totalAllocatedBytes":128,"allocationEvents":2,"liveBytes":64,"liveAllocations":1,"peakLiveBytes":128}},"dropped":{{"allocationRecords":0,"allocationStacks":0,"processSamples":0,"unknownFrees":0}},"processMemorySamples":[],"stacks":[{{"stack":["0x1000"],"totalAllocatedBytes":128,"allocationCount":2,"liveBytes":64,"liveAllocations":1,"peakLiveBytes":128}}]}}}}
JSON
      ;;
    *)
      cat > "$ORBI_TRACE_OUTPUT" <<'JSON'
{{"format":"orbi.trace.v1","formatVersion":1,"mode":"cpu","startedAtUnixNanos":1,"host":"mock","cpu":{{"sampleIntervalMicros":4500,"droppedSamples":0,"failedUnwinds":0,"processMemorySamples":[],"threads":[{{"id":1,"name":"main","samples":[{{"timeNanos":1,"stack":["0x1000"]}}]}}]}}}}
JSON
      ;;
  esac
fi
trap 'exit 0' INT TERM
while :; do
  sleep 1
done
SCRIPT
  chmod +x "$out"
  if [ -n "$module" ]; then
    mkdir -p "$(dirname "$module")"
    : > "$module"
  fi
  exit 0
fi
if [ "$#" -ge 3 ] && [ "$1" = "--sdk" ] && {{ [ "$3" = "clang" ] || [ "$3" = "clang++" ]; }}; then
  out=""
  depfile=""
  source=""
  prev=""
  for arg in "$@"; do
    if [ "$prev" = "-o" ]; then
      out="$arg"
    fi
    if [ "$prev" = "-MF" ]; then
      depfile="$arg"
    fi
    if [ "$prev" = "-c" ]; then
      source="$arg"
    fi
    prev="$arg"
  done
  if [ -n "$out" ]; then
    mkdir -p "$(dirname "$out")"
    : > "$out"
  fi
  if [ -n "$depfile" ] && [ -n "$out" ]; then
    mkdir -p "$(dirname "$depfile")"
    deps="$source"
    if [ -n "$source" ] && [ -f "$source" ]; then
      source_dir="$(dirname "$source")"
      for header in "$source_dir"/*.h "$source_dir"/*.hh "$source_dir"/*.hpp "$source_dir"/*.hxx; do
        if [ -f "$header" ]; then
          deps="$deps $header"
        fi
      done
    fi
    printf '%s: %s\n' "$out" "$deps" > "$depfile"
  fi
  exit 0
fi
if [ "$#" -ge 1 ] && [ "$1" = "lipo" ]; then
  out=""
  prev=""
  for arg in "$@"; do
    if [ "$prev" = "-output" ]; then
      out="$arg"
    fi
    prev="$arg"
  done
  if [ -n "$out" ]; then
    mkdir -p "$(dirname "$out")"
    : > "$out"
  fi
  exit 0
fi
if [ "$#" -ge 1 ] && [ "$1" = "actool" ]; then
  compile_dir=""
  partial=""
  app_icon=0
  prev=""
  for arg in "$@"; do
    if [ "$prev" = "--compile" ]; then
      compile_dir="$arg"
    fi
    if [ "$prev" = "--output-partial-info-plist" ]; then
      partial="$arg"
    fi
    if [ "$prev" = "--app-icon" ]; then
      app_icon=1
    fi
    prev="$arg"
  done
  mkdir -p "$compile_dir"
  : > "$compile_dir/Assets.car"
  if [ "$app_icon" -eq 1 ]; then
    : > "$compile_dir/AppIcon60x60@2x.png"
    : > "$compile_dir/AppIcon76x76@2x~ipad.png"
    cat > "$partial" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIcons</key>
  <dict>
    <key>CFBundlePrimaryIcon</key>
    <dict>
      <key>CFBundleIconFiles</key>
      <array>
        <string>AppIcon60x60</string>
      </array>
      <key>CFBundleIconName</key>
      <string>AppIcon</string>
    </dict>
  </dict>
  <key>CFBundleIcons~ipad</key>
  <dict>
    <key>CFBundlePrimaryIcon</key>
    <dict>
      <key>CFBundleIconFiles</key>
      <array>
        <string>AppIcon60x60</string>
        <string>AppIcon76x76</string>
      </array>
      <key>CFBundleIconName</key>
      <string>AppIcon</string>
    </dict>
  </dict>
</dict>
</plist>
PLIST
  else
    cat > "$partial" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict/>
</plist>
PLIST
  fi
  exit 0
fi
{extra_commands}
echo "unexpected xcrun command: $@" >&2
exit 1
"#,
            sdk = sdk_root.display(),
            sdk_version_block = sdk_version_block,
            extra_commands = extra_commands,
        ),
    );
}
