#!/usr/bin/env bash
# deploy/test.sh: runs deploy.sh's swap, gates and rollback against a
# throwaway systemd-run user unit. the "builds" are python stubs that
# write a tap in transmog's layout, listen on two free high ports in place
# of 5960/5961 and write control.state; one of them misbehaves the way a
# real build did. nothing is built, and no real transmog unit is touched.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
UNIT=transmog-deploy-test
py=$(command -v python3) || {
	echo "test: needs python3 on PATH, e.g. nix shell nixpkgs#python3 -c deploy/test.sh" >&2
	exit 1
}

tmp=$(mktemp -d /tmp/transmog-deploy-test.XXXXXX)
cleanup() {
	systemctl --user stop "$UNIT.service" >/dev/null 2>&1 || true
	systemctl --user reset-failed "$UNIT.service" >/dev/null 2>&1 || true
	case $tmp in
	/tmp/transmog-deploy-test.??????) rm -rf -- "$tmp" ;;
	esac
}
trap cleanup EXIT

port_free() { [[ -z $(ss -Htan "( sport = :$1 )") ]]; }
while :; do
	p1=$((20000 + RANDOM % 40000))
	port_free "$p1" && port_free $((p1 + 1)) && port_free $((p1 + 2)) && break
done
p2=$((p1 + 1))

mkdir -p "$tmp/stubs" "$tmp/bin" "$tmp/state"
cat >"$tmp/stubs/stub.py" <<'EOF'
import os, socket, struct, sys, time

mode = os.path.basename(sys.argv[0]).split("-")[1].rstrip("0123456789")
args = sys.argv[1:]
tap = args[args.index("--tap") + 1]
ws = args[args.index("--control") + 1] if "--control" in args else None
p1, p2 = map(int, os.environ["STUB_PORTS"].split())
if mode == "wrongport":
    # what the ndi sdk does while the old socket still holds the second port
    p2 += 1
fps = 22 if mode == "slow" else 30

socks = []
for port in (p1, p2):
    s = socket.socket()
    s.bind(("127.0.0.1", port))
    s.listen()
    socks.append(s)

os.makedirs(tap, exist_ok=True)
for name in os.listdir(tap):
    if name.endswith(".frame") or name == "latest":
        os.remove(os.path.join(tap, name))
if ws:
    state = os.path.join(tap, "control.state")
    with open(state + ".tmp", "w") as f:
        f.write(f"t {time.time():.6f}\nqa_lock 1\nndi on\nws {ws} connected\n")
    os.rename(state + ".tmp", state)

def put(path, data):
    with open(path + ".tmp", "wb") as f:
        f.write(data)
    os.rename(path + ".tmp", path)

slots, seq, start = 64, 0, time.monotonic()
while True:
    seq += 1
    ts = time.monotonic_ns()
    header = struct.pack("<8sQIIqqII4sI8x", b"TMQATAP1", seq, seq + 1000, 0,
                         ts, ts, 1920, 1080, b"MJPG", 4)
    put(os.path.join(tap, f"{seq % slots:03d}.frame"), header + b"\xff\xd8\xff\xd9")
    put(os.path.join(tap, "latest"), f"{seq} {slots}\n".encode())
    if mode == "dies" and seq == 45:
        sys.exit(1)
    time.sleep(max(0.0, start + seq / fps - time.monotonic()))
EOF
for name in good0 good1 slow wrongport dies; do
	{
		printf '#!%s -I\n' "$py"
		cat "$tmp/stubs/stub.py"
	} >"$tmp/stubs/transmog-$name"
	chmod +x "$tmp/stubs/transmog-$name"
	ln -s "$tmp/stubs/transmog-$name" "$tmp/bin/transmog-$name"
done

{
	printf 'BIN_DIR=%q\n' "$tmp/bin"
	printf 'UNIT=%q\n' "$UNIT"
	printf 'STATE_DIR=%q\n' "$tmp/state"
	printf 'DEPLOY_LOG=%q\n' "$tmp/deploy.log"
	printf 'NDI_PORTS=%q\n' "$p1 $p2"
	printf 'HEALTH_SECS=3\nSTARTUP_SECS=5\n'
	# shellcheck disable=SC2016 # deploy.sh expands these when it runs them
	printf 'HEALTH_CMDS=(%q %q)\n' 'kill -0 "$DEPLOY_PID"' 'test -s "$TAP_DIR/latest"'
	printf 'LOG_HOOK=%q\n' "printf '%s\\n' \"\$1\" >>$tmp/hook.log"
} >"$tmp/deploy.env"

# the bad build's health check takes away write access to BIN_DIR and the
# log, the way a filesystem gone read-only would, before it fails
{
	printf 'source %q\n' "$tmp/deploy.env"
	printf 'HEALTH_CMDS=(%q)\n' \
		"case \$DEPLOY_BIN in */transmog-good0) chmod a-w $tmp/bin $tmp/deploy.log; exit 1 ;; esac"
} >"$tmp/readonly.env"

qa_before=$(systemctl --user show -P MainPID transmog-qa.service 2>/dev/null || true)

systemd-run --user --quiet --unit "$UNIT" -p WorkingDirectory="$tmp" \
	-p StandardOutput=append:"$tmp/unit.log" -p StandardError=append:"$tmp/unit.log" \
	-E STUB_PORTS="$p1 $p2" "$tmp/bin/transmog-good0" \
	gui --tap "$tmp/tap" --preset qa --ndi --control ws://127.0.0.1:9/viz_ctrl/ws

failures=0
check() {
	local what=$1
	shift
	if "$@"; then
		echo "  ok   $what"
	else
		echo "  FAIL $what"
		failures=$((failures + 1))
	fi
}

running() {
	grep -q "^ExecStart=\".*/transmog-$1\"" \
		"$(systemctl --user show -P FragmentPath "$UNIT.service")" &&
		[[ $(systemctl --user show -P ActiveState "$UNIT.service") == active ]]
}

points() { [[ $(readlink "$tmp/bin/$1") == "$tmp/bin/transmog-$2" ]]; }

# deploy BUILD [ENV]: deploy.sh's deploy_bin on BUILD, settings from ENV
deploy() {
	local rc=0
	DEPLOY_ENV=$tmp/${2:-deploy}.env bash -c 'source "$1"; load_env; deploy_bin "$2"' \
		deploy "$here/deploy.sh" "$tmp/bin/transmog-$1" 2>>"$tmp/stderr.log" || rc=$?
	echo "$rc"
}

echo "gates pass: good0 -> good1"
rc=$(deploy good1)
check "exit 0" [ "$rc" = 0 ]
check "current -> good1" points current good1
check "previous -> good0" points previous good0
check "unit runs good1" running good1

for bad in slow wrongport dies; do
	echo "$bad build rolls back"
	rc=$(deploy "$bad")
	check "exit 1" [ "$rc" = 1 ]
	check "current -> good1" points current good1
	check "failed -> $bad" points failed "$bad"
	check "unit runs good1 again" running good1
done

check "fps gate named in the log" grep -q 'gate failed: tap: 2[0-9.]* fps, below 29' "$tmp/deploy.log"
check "port gate named in the log" grep -q "gate failed: port $p2: listener none" "$tmp/deploy.log"
check "process gate named in the log" grep -q 'gate failed: process: ' "$tmp/deploy.log"
check "log hook saw three rollbacks" [ "$(grep -c '^rolled back' "$tmp/hook.log")" = 3 ]
echo "rollback with BIN_DIR and the log read-only"
rc=$(deploy good0 readonly)
chmod u+w "$tmp/bin" "$tmp/deploy.log"
check "exit 2" [ "$rc" = 2 ]
check "unit runs good1 again" running good1
check "current left on good0" points current good0
check "summary says the links are wrong" grep -q 'links in .* could not be fixed' "$tmp/hook.log"

qa_after=$(systemctl --user show -P MainPID transmog-qa.service 2>/dev/null || true)
check "transmog-qa untouched" [ "$qa_before" = "$qa_after" ]

if ((failures > 0)); then
	echo "$failures check(s) failed; deploy log:"
	cat "$tmp/deploy.log"
	exit 1
fi
echo "all checks passed"
